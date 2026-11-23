//! 主机配置与认证模型（对应 01/02/03 画板）
//!
//! 主机配置持久化到 `~/Library/Application Support/HoloSSH/hosts.json`，
//! 密码绝不明文写入 JSON，统一由 `services/vault.rs` 存入跨平台加密密钥库。

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// 当前 UTC 毫秒时间戳（用于多设备 CRDT 合并与删除墓碑）
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 生成 RFC 4122 UUID v4 作为全局唯一主机 ID，彻底避免多设备同步撞 ID
pub fn generate_host_uuid() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3],
        b[4], b[5],
        b[6], b[7],
        b[8], b[9],
        b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// 连接方式（对应 02 新建主机弹窗「连接」下拉框）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ConnectionKind {
    #[default]
    Direct,
    Local,
    JumpHost,
    Socks5,
    HttpProxy,
}

impl ConnectionKind {
    pub const ALL: [Self; 5] = [
        Self::Direct,
        Self::Local,
        Self::JumpHost,
        Self::Socks5,
        Self::HttpProxy,
    ];

    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Direct => crate::i18n::tr("enum.conn.direct"),
            Self::Local => crate::i18n::tr("enum.conn.local"),
            Self::JumpHost => crate::i18n::tr("enum.conn.jump"),
            Self::Socks5 => crate::i18n::tr("enum.conn.socks5"),
            Self::HttpProxy => crate::i18n::tr("enum.conn.http"),
        }
    }
}

/// 5 种身份验证方式（对应 02 新建主机弹窗 5 项分段选择器）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AuthMethod {
    #[default]
    Auto,
    Password,
    PrivateKey,
    Agent,
    KeyboardInteractive,
}

impl AuthMethod {
    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Auto => crate::i18n::tr("enum.auth.auto"),
            Self::Password => crate::i18n::tr("enum.auth.password"),
            Self::PrivateKey => crate::i18n::tr("enum.auth.key"),
            Self::Agent => crate::i18n::tr("enum.auth.agent"),
            Self::KeyboardInteractive => crate::i18n::tr("enum.auth.interactive"),
        }
    }

    pub fn icon_path(self) -> &'static str {
        match self {
            Self::Auto => "icons/auth-auto.svg",
            Self::Password => "icons/auth-password.svg",
            Self::PrivateKey => "icons/auth-key.svg",
            Self::Agent => "icons/auth-agent.svg",
            Self::KeyboardInteractive => "icons/auth-interactive.svg",
        }
    }
}

/// 主机在线状态（对应侧边栏状态圆点）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HostStatus {
    Connected,
    #[default]
    Idle,
}

/// 单台 SSH 主机配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostProfile {
    pub id: String,
    pub name: String,
    pub group: String,
    pub connection: ConnectionKind,
    /// 跳板机关联的已添加主机 ID（对应 Tabby `jumpHost`，支持多级跳板链与自动取用跳板机密钥库凭据）
    #[serde(default)]
    pub jump_host_id: Option<String>,
    /// SOCKS5 / HTTP 代理主机地址（与端口分栏填写，对应 Tabby `socksProxyHost` / `httpProxyHost`）
    #[serde(default)]
    pub proxy_host: Option<String>,
    /// SOCKS5 / HTTP 代理端口（对应 Tabby `socksProxyPort` / `httpProxyPort`）
    #[serde(default)]
    pub proxy_port: Option<u16>,
    pub hostname: String,
    pub port: u16,
    pub username: String,
    pub auth_method: AuthMethod,
    /// 私钥路径（密码本身存入加密密钥库，不落明文配置）
    pub private_key_path: Option<String>,
    /// 添加时间（UTC 毫秒，用于首页列表默认按添加时间倒序排序及拖拽置顶持久化）
    #[serde(default)]
    pub created_at_ms: u64,
    /// 最后修改时间（UTC 毫秒，用于跨设备 LWW 合并）
    #[serde(default)]
    pub updated_at_ms: u64,
    #[serde(skip)]
    pub status: HostStatus,
}

impl HostProfile {
    /// 是否为本机原生 PTY 终端会话（无需远端 SSH 地址、凭据或 SFTP）
    #[inline]
    pub fn is_local(&self) -> bool {
        self.connection == ConnectionKind::Local
    }

    /// 侧边栏行右侧副标题：本地终端显示默认 Shell 名（如 `zsh`），远端主机显示主机地址
    pub fn sidebar_subtitle(&self) -> String {
        if self.is_local() {
            crate::services::local_pty::default_local_shell_name()
        } else {
            self.hostname.clone()
        }
    }

    /// 用于列表倒序排序的有效添加时间戳（兼容未记录 created_at_ms 的旧主机，回退至 updated_at_ms）
    #[inline]
    pub fn effective_created_at_ms(&self) -> u64 {
        if self.created_at_ms > 0 {
            self.created_at_ms
        } else {
            self.updated_at_ms
        }
    }
    /// 获取 SOCKS5 / HTTP 代理的 `(host, port)`
    pub fn effective_proxy_host_port(&self) -> Option<(String, u16)> {
        let default_port = match self.connection {
            ConnectionKind::Socks5 => 1080,
            ConnectionKind::HttpProxy => 8080,
            _ => 1080,
        };
        let host = self
            .proxy_host
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        let clean_host = host
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_start_matches("socks5://")
            .trim_end_matches('/');
        Some((
            clean_host.to_string(),
            self.proxy_port.filter(|&p| p > 0).unwrap_or(default_port),
        ))
    }

    /// 获取用于跳板机下拉展示的标签（与 Tabby `分组 / 名称` 规范一致）
    pub fn jump_display_label(&self) -> String {
        if self.group.trim().is_empty() {
            self.name.clone()
        } else {
            format!("{} / {}", self.group.trim(), self.name)
        }
    }

    fn store_path() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join("Holossh").join("hosts.json"))
    }

    /// 从本地持久化文件加载真实主机列表；主文件异常时自动回退 `.bak` 备份
    pub fn load_persisted() -> Vec<Self> {
        let Some(path) = Self::store_path() else {
            return Vec::new();
        };
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(list) = serde_json::from_str::<Vec<Self>>(&raw) {
                return list;
            }
        }
        let bak_path = path.with_extension("json.bak");
        if let Ok(raw) = fs::read_to_string(&bak_path) {
            if let Ok(list) = serde_json::from_str::<Vec<Self>>(&raw) {
                return list;
            }
        }
        Vec::new()
    }

    /// 原子持久化主机列表到本地 JSON（临时文件 + fsync + 保留 `.bak` + rename，防止断电截断）
    pub fn save_all(hosts: &[Self]) {
        let Some(path) = Self::store_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let Ok(json) = serde_json::to_string_pretty(hosts) else {
            return;
        };
        atomic_write_json(&path, json.as_bytes());
    }
}

fn atomic_write_json(path: &PathBuf, bytes: &[u8]) {
    use std::io::Write;
    let tmp_path = path.with_extension("json.tmp");
    let bak_path = path.with_extension("json.bak");
    if let Ok(mut f) = fs::File::create(&tmp_path) {
        if f.write_all(bytes).is_ok() && f.sync_all().is_ok() {
            if path.exists() {
                let _ = fs::copy(path, &bak_path);
            }
            let _ = fs::rename(&tmp_path, path);
        } else {
            let _ = fs::remove_file(&tmp_path);
        }
    }
}
