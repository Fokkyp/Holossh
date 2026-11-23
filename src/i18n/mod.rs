//! 全局多语言（i18n）引擎：从 `assets/i18n/*.json` 解耦加载全部界面文案、枚举标签与 Toast 提示
//!
//! - 默认从编译期打包的 `assets/i18n/{zh-Hans,zh-Hant,en,ja,ko}.json` 解析并缓存于 `OnceLock`；
//! - 若 `~/Library/Application Support/HoloSSH/i18n/<code>.json` 存在外部覆盖文件，则自动叠加覆盖；
//! - 通过 `set_language(lang)` 与设置页语言实时同步，全应用任意视图与后台服务均可调用 `tr(key)` / `trf(key, vars)`。

use crate::state::settings::{AppSettings, Language};
use std::collections::HashMap;
use std::fs;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

const ZH_HANS_JSON: &str = include_str!("../../assets/i18n/zh-Hans.json");
const ZH_HANT_JSON: &str = include_str!("../../assets/i18n/zh-Hant.json");
const EN_JSON: &str = include_str!("../../assets/i18n/en.json");
const JA_JSON: &str = include_str!("../../assets/i18n/ja.json");
const KO_JSON: &str = include_str!("../../assets/i18n/ko.json");

struct I18nCatalog {
    bundles: [HashMap<String, String>; 5],
}

static CATALOG: OnceLock<I18nCatalog> = OnceLock::new();
static CURRENT_LANG: AtomicU8 = AtomicU8::new(0);

fn lang_index(lang: Language) -> usize {
    match lang {
        Language::ZhHans => 0,
        Language::ZhHant => 1,
        Language::English => 2,
        Language::Japanese => 3,
        Language::Korean => 4,
    }
}

fn lang_from_index(idx: u8) -> Language {
    match idx {
        1 => Language::ZhHant,
        2 => Language::English,
        3 => Language::Japanese,
        4 => Language::Korean,
        _ => Language::ZhHans,
    }
}

fn load_bundle(raw: &str, code: &str) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = serde_json::from_str(raw).unwrap_or_default();
    // 支持外部 i18n 配置文件热覆盖（位于配置目录 i18n/<code>.json）
    if let Some(dir) = AppSettings::config_dir() {
        let override_path = dir.join("i18n").join(format!("{code}.json"));
        if let Ok(text) = fs::read_to_string(&override_path) {
            if let Ok(extra) = serde_json::from_str::<HashMap<String, String>>(&text) {
                for (k, v) in extra {
                    map.insert(k, v);
                }
            }
        }
    }
    map
}

fn catalog() -> &'static I18nCatalog {
    CATALOG.get_or_init(|| I18nCatalog {
        bundles: [
            load_bundle(ZH_HANS_JSON, Language::ZhHans.code()),
            load_bundle(ZH_HANT_JSON, Language::ZhHant.code()),
            load_bundle(EN_JSON, Language::English.code()),
            load_bundle(JA_JSON, Language::Japanese.code()),
            load_bundle(KO_JSON, Language::Korean.code()),
        ],
    })
}

/// 切换当前全局语言（由启动加载与设置页语言下拉框调用）
pub fn set_language(lang: Language) {
    CURRENT_LANG.store(lang_index(lang) as u8, Ordering::Relaxed);
}

/// 获取当前激活语言
pub fn current_language() -> Language {
    lang_from_index(CURRENT_LANG.load(Ordering::Relaxed))
}

/// 读取指定语言的静态翻译文案（缺失时自动回退至 zh-Hans）
pub fn t(lang: Language, key: &str) -> &'static str {
    let cat = catalog();
    if let Some(v) = cat.bundles[lang_index(lang)].get(key) {
        return v.as_str();
    }
    if let Some(v) = cat.bundles[0].get(key) {
        return v.as_str();
    }
    ""
}

/// 读取指定语言的模板翻译文案并替换 `{var}` 占位符
pub fn tf(lang: Language, key: &str, vars: &[(&str, &str)]) -> String {
    let mut out = t(lang, key).to_string();
    for (name, val) in vars {
        let pat = format!("{{{name}}}");
        out = out.replace(&pat, val);
    }
    out
}

/// 读取当前全局语言的翻译文案
pub fn tr(key: &str) -> &'static str {
    t(current_language(), key)
}

/// 读取当前全局语言的模板翻译文案并替换 `{var}` 占位符
pub fn trf(key: &str, vars: &[(&str, &str)]) -> String {
    tf(current_language(), key, vars)
}

/// 读取指定语言在 i18n 配置文件中定义的显示名称（`meta.language_name`）
pub fn language_name(lang: Language) -> &'static str {
    t(lang, "meta.language_name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_locale_files_parse_with_full_key_parity() {
        let raw_files = [
            ("zh-Hans", ZH_HANS_JSON),
            ("zh-Hant", ZH_HANT_JSON),
            ("en", EN_JSON),
            ("ja", JA_JSON),
            ("ko", KO_JSON),
        ];
        let parsed: Vec<(&str, HashMap<String, String>)> = raw_files
            .iter()
            .map(|(code, raw)| {
                let m: HashMap<String, String> =
                    serde_json::from_str(raw).expect("valid locale JSON");
                assert!(!m.is_empty(), "{code} must not be empty");
                (*code, m)
            })
            .collect();

        let base = &parsed[0].1;
        for (code, map) in &parsed[1..] {
            for key in base.keys() {
                let val = map
                    .get(key)
                    .unwrap_or_else(|| panic!("missing key '{key}' in {code}.json"));
                assert!(!val.is_empty(), "empty value for '{key}' in {code}.json");
            }
            assert_eq!(
                map.len(),
                base.len(),
                "extra or missing keys in {code}.json"
            );
        }

        assert_eq!(language_name(Language::English), "English");
        assert_eq!(language_name(Language::Japanese), "日本語");
        assert_eq!(
            tf(
                Language::English,
                "toast.host_proxy_host_required",
                &[("conn", "SOCKS5")]
            ),
            "Please enter the SOCKS5 host address"
        );
    }
}
