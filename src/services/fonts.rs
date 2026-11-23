//! 应用内置云端字体管理服务（不安装到 macOS 系统字体库，仅注册至 HoloSSH 私有内存字体源）
//!
//! - 「系统默认」直接使用 macOS 原生 Menlo 等宽字体；
//! - 其余开源编程等宽字体从官方开源仓库实时流式下载至
//!   `~/Library/Application Support/HoloSSH/fonts/<id>/`，
//!   启动与下载完成时通过 GPUI `TextSystem::add_fonts` 仅在当前进程内加载。

use crate::i18n::{tr, trf};
use crate::services::ssh::runtime;
use crate::state::settings::AppSettings;
use std::borrow::Cow;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const SYSTEM_DEFAULT_FONT_KEY: &str = "系统默认";
pub const SYSTEM_DEFAULT_GPUI_FAMILY: &str = "Menlo";

/// 单个官方远程字体文件源
#[derive(Debug, Clone, Copy)]
pub struct FontFileSource {
    pub filename: &'static str,
    pub url: &'static str,
}

/// 可选终端等宽字体目录条目
#[derive(Debug, Clone, Copy)]
pub struct FontSpec {
    pub id: &'static str,
    pub display_name: &'static str,
    pub gpui_family: &'static str,
    pub is_system: bool,
    pub files: &'static [FontFileSource],
}

/// 终端字体官方清单（第 1 项为系统默认，其余均为官方开源仓库直链）
pub const FONT_CATALOG: &[FontSpec] = &[
    FontSpec {
        id: "system",
        display_name: SYSTEM_DEFAULT_FONT_KEY,
        gpui_family: SYSTEM_DEFAULT_GPUI_FAMILY,
        is_system: true,
        files: &[],
    },
    FontSpec {
        id: "jetbrains-mono",
        display_name: "JetBrains Mono",
        gpui_family: "JetBrains Mono",
        is_system: false,
        files: &[
            FontFileSource {
                filename: "JetBrainsMono-Regular.ttf",
                url: "https://raw.githubusercontent.com/JetBrains/JetBrainsMono/master/fonts/ttf/JetBrainsMono-Regular.ttf",
            },
            FontFileSource {
                filename: "JetBrainsMono-Bold.ttf",
                url: "https://raw.githubusercontent.com/JetBrains/JetBrainsMono/master/fonts/ttf/JetBrainsMono-Bold.ttf",
            },
        ],
    },
    FontSpec {
        id: "fira-code",
        display_name: "Fira Code",
        gpui_family: "Fira Code",
        is_system: false,
        files: &[FontFileSource {
            filename: "FiraCode-Variable.ttf",
            url: "https://raw.githubusercontent.com/google/fonts/main/ofl/firacode/FiraCode%5Bwght%5D.ttf",
        }],
    },
    FontSpec {
        id: "source-code-pro",
        display_name: "Source Code Pro",
        gpui_family: "Source Code Pro",
        is_system: false,
        files: &[
            FontFileSource {
                filename: "SourceCodePro-Regular.ttf",
                url: "https://raw.githubusercontent.com/adobe-fonts/source-code-pro/release/TTF/SourceCodePro-Regular.ttf",
            },
            FontFileSource {
                filename: "SourceCodePro-Bold.ttf",
                url: "https://raw.githubusercontent.com/adobe-fonts/source-code-pro/release/TTF/SourceCodePro-Bold.ttf",
            },
        ],
    },
    FontSpec {
        id: "ibm-plex-mono",
        display_name: "IBM Plex Mono",
        gpui_family: "IBM Plex Mono",
        is_system: false,
        files: &[
            FontFileSource {
                filename: "IBMPlexMono-Regular.ttf",
                url: "https://raw.githubusercontent.com/google/fonts/main/ofl/ibmplexmono/IBMPlexMono-Regular.ttf",
            },
            FontFileSource {
                filename: "IBMPlexMono-Bold.ttf",
                url: "https://raw.githubusercontent.com/google/fonts/main/ofl/ibmplexmono/IBMPlexMono-Bold.ttf",
            },
        ],
    },
    FontSpec {
        id: "hack",
        display_name: "Hack",
        gpui_family: "Hack",
        is_system: false,
        files: &[
            FontFileSource {
                filename: "Hack-Regular.ttf",
                url: "https://raw.githubusercontent.com/source-foundry/Hack/master/build/ttf/Hack-Regular.ttf",
            },
            FontFileSource {
                filename: "Hack-Bold.ttf",
                url: "https://raw.githubusercontent.com/source-foundry/Hack/master/build/ttf/Hack-Bold.ttf",
            },
        ],
    },
    FontSpec {
        id: "ubuntu-mono",
        display_name: "Ubuntu Mono",
        gpui_family: "Ubuntu Mono",
        is_system: false,
        files: &[
            FontFileSource {
                filename: "UbuntuMono-Regular.ttf",
                url: "https://raw.githubusercontent.com/google/fonts/main/ufl/ubuntumono/UbuntuMono-Regular.ttf",
            },
            FontFileSource {
                filename: "UbuntuMono-Bold.ttf",
                url: "https://raw.githubusercontent.com/google/fonts/main/ufl/ubuntumono/UbuntuMono-Bold.ttf",
            },
        ],
    },
];

/// 后台字体下载回传给 UI 的实时事件
#[derive(Debug, Clone)]
pub enum FontDownloadEvent {
    Progress {
        font_id: String,
        progress: f32,
    },
    Completed {
        font_id: String,
        gpui_family: String,
        font_bytes: Vec<Vec<u8>>,
    },
    Failed {
        font_id: String,
        error: String,
    },
}

/// 应用私有字体缓存根目录（绝不写入系统 ~/Library/Fonts）
pub fn app_fonts_dir() -> Option<PathBuf> {
    AppSettings::config_dir().map(|d| d.join("fonts"))
}

pub fn font_dir_for(font_id: &str) -> Option<PathBuf> {
    app_fonts_dir().map(|d| d.join(font_id))
}

#[cfg(test)]
pub fn find_spec_by_id(font_id: &str) -> Option<&'static FontSpec> {
    FONT_CATALOG.iter().find(|s| s.id == font_id)
}

pub fn find_spec_by_stored_name(stored: &str) -> &'static FontSpec {
    if stored == SYSTEM_DEFAULT_FONT_KEY || stored == "system" {
        return &FONT_CATALOG[0];
    }
    FONT_CATALOG
        .iter()
        .find(|s| s.display_name == stored || s.gpui_family == stored || s.id == stored)
        .unwrap_or(&FONT_CATALOG[0])
}

/// 判断指定字体是否已完整下载到本应用私有缓存中
pub fn is_font_downloaded(spec: &FontSpec) -> bool {
    if spec.is_system {
        return true;
    }
    let Some(dir) = font_dir_for(spec.id) else {
        return false;
    };
    spec.files.iter().all(|f| {
        let p = dir.join(f.filename);
        fs::metadata(p).map(|m| m.len() > 8_192).unwrap_or(false)
    })
}

/// 启动时读取所有已下载的应用私有字体字节，注册至 GPUI 进程内存（不触碰系统字体）
pub fn load_all_downloaded_font_bytes() -> Vec<Cow<'static, [u8]>> {
    let mut loaded = Vec::new();
    for spec in FONT_CATALOG {
        if spec.is_system || !is_font_downloaded(spec) {
            continue;
        }
        if let Some(dir) = font_dir_for(spec.id) {
            for f in spec.files {
                if let Ok(bytes) = fs::read(dir.join(f.filename)) {
                    if is_valid_font_header(&bytes) {
                        loaded.push(Cow::Owned(bytes));
                    }
                }
            }
        }
    }
    loaded
}

/// 将设置中的字体名解析为展示文案
pub fn display_name_for_setting(stored: &str) -> &'static str {
    let spec = find_spec_by_stored_name(stored);
    if spec.is_system {
        tr("common.system_default")
    } else {
        spec.display_name
    }
}

/// 将设置中的字体名解析为 GPUI 实际渲染字体家族名（未下载时安全回退系统默认）
pub fn effective_gpui_family(stored: &str) -> &'static str {
    let spec = find_spec_by_stored_name(stored);
    if spec.is_system || is_font_downloaded(spec) {
        spec.gpui_family
    } else {
        SYSTEM_DEFAULT_GPUI_FAMILY
    }
}

/// 校验 TrueType / OpenType 字体魔数头，避免将错误 HTML 页写入缓存
fn is_valid_font_header(bytes: &[u8]) -> bool {
    if bytes.len() < 12 {
        return false;
    }
    matches!(
        &bytes[0..4],
        [0x00, 0x01, 0x00, 0x00] | b"OTTO" | b"true" | b"ttcf"
    )
}

/// 在后台 Tokio 线程池中从官方源流式下载指定字体至应用私有目录，并实时上报进度
pub fn spawn_font_download(spec: &'static FontSpec, tx: mpsc::Sender<FontDownloadEvent>) {
    if spec.is_system || spec.files.is_empty() {
        return;
    }
    runtime().spawn(async move {
        let Some(target_dir) = font_dir_for(spec.id) else {
            let _ = tx.send(FontDownloadEvent::Failed {
                font_id: spec.id.to_string(),
                error: tr("svc.vault.no_dir").into(),
            });
            return;
        };
        if let Err(e) = fs::create_dir_all(&target_dir) {
            let _ = tx.send(FontDownloadEvent::Failed {
                font_id: spec.id.to_string(),
                error: trf("svc.fonts.mkdir_err", &[("err", &e.to_string())]),
            });
            return;
        }

        let client = match reqwest::Client::builder()
            .user_agent(concat!("Holossh/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(60))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(FontDownloadEvent::Failed {
                    font_id: spec.id.to_string(),
                    error: trf("svc.fonts.client_err", &[("err", &e.to_string())]),
                });
                return;
            }
        };

        let file_count = spec.files.len() as f32;
        let mut all_bytes: Vec<Vec<u8>> = Vec::with_capacity(spec.files.len());
        let mut last_emit = Instant::now()
            .checked_sub(Duration::from_millis(60))
            .unwrap_or_else(Instant::now);

        let _ = tx.send(FontDownloadEvent::Progress {
            font_id: spec.id.to_string(),
            progress: 0.04,
        });

        for (file_idx, file_src) in spec.files.iter().enumerate() {
            let mut resp = match client.get(file_src.url).send().await {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => {
                    let _ = tx.send(FontDownloadEvent::Failed {
                        font_id: spec.id.to_string(),
                        error: trf(
                            "svc.fonts.http_err",
                            &[
                                ("name", spec.display_name),
                                ("code", &r.status().as_u16().to_string()),
                            ],
                        ),
                    });
                    return;
                }
                Err(e) => {
                    let _ = tx.send(FontDownloadEvent::Failed {
                        font_id: spec.id.to_string(),
                        error: trf(
                            "svc.fonts.net_err",
                            &[("name", spec.display_name), ("err", &e.to_string())],
                        ),
                    });
                    return;
                }
            };

            let total_len = resp.content_length().unwrap_or(280_000).max(1);
            let mut buf = Vec::with_capacity(total_len as usize);
            let base_frac = file_idx as f32 / file_count;
            let file_span = 1.0 / file_count;

            loop {
                match resp.chunk().await {
                    Ok(Some(chunk)) => {
                        buf.extend_from_slice(&chunk);
                        if last_emit.elapsed() >= Duration::from_millis(32) {
                            last_emit = Instant::now();
                            let local_frac =
                                (buf.len() as f32 / total_len as f32).clamp(0.0, 0.98);
                            let overall = (base_frac + local_frac * file_span).clamp(0.05, 0.99);
                            let _ = tx.send(FontDownloadEvent::Progress {
                                font_id: spec.id.to_string(),
                                progress: overall,
                            });
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        let _ = tx.send(FontDownloadEvent::Failed {
                            font_id: spec.id.to_string(),
                            error: trf(
                                "svc.fonts.read_err",
                                &[("name", spec.display_name), ("err", &e.to_string())],
                            ),
                        });
                        return;
                    }
                }
            }

            if !is_valid_font_header(&buf) {
                let _ = tx.send(FontDownloadEvent::Failed {
                    font_id: spec.id.to_string(),
                    error: trf("svc.fonts.verify_err", &[("name", spec.display_name)]),
                });
                return;
            }

            let final_path = target_dir.join(file_src.filename);
            let tmp_path = target_dir.join(format!("{}.tmp", file_src.filename));
            if let Err(e) = fs::write(&tmp_path, &buf).and_then(|_| fs::rename(&tmp_path, &final_path))
            {
                let _ = fs::remove_file(&tmp_path);
                let _ = tx.send(FontDownloadEvent::Failed {
                    font_id: spec.id.to_string(),
                    error: trf("svc.fonts.save_err", &[("err", &e.to_string())]),
                });
                return;
            }
            all_bytes.push(buf);
        }

        let _ = tx.send(FontDownloadEvent::Completed {
            font_id: spec.id.to_string(),
            gpui_family: spec.gpui_family.to_string(),
            font_bytes: all_bytes,
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_system_and_catalog_fonts_cleanly() {
        assert_eq!(display_name_for_setting("系统默认"), "系统默认");
        assert_eq!(effective_gpui_family("系统默认"), "Menlo");
        assert_eq!(
            find_spec_by_id("jetbrains-mono").unwrap().gpui_family,
            "JetBrains Mono"
        );
        assert!(is_valid_font_header(&[
            0x00, 0x01, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0
        ]));
        assert!(!is_valid_font_header(b"<html>404</html>"));
    }
}
