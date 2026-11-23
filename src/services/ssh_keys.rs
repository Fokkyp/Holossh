//! 本机 SSH 私钥探测与路径展开工具（纯跨平台文件扫描，不依赖 macOS Keychain）

use std::fs;
use std::path::PathBuf;

/// 扫描本机 `~/.ssh/` 下的私钥文件清单（供设置页「密钥库」只读展示与自动认证探测）
pub fn discover_local_ssh_keys() -> Vec<PathBuf> {
    let Some(ssh_dir) = dirs::home_dir().map(|h| h.join(".ssh")) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(ssh_dir) else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with(".pub")
            || matches!(
                name,
                "config" | "known_hosts" | "known_hosts.old" | "authorized_keys"
            )
        {
            continue;
        }
        if name.starts_with("id_") || name.ends_with(".pem") || name.ends_with(".key") {
            keys.push(path);
        }
    }
    keys.sort();
    keys
}

/// 展开路径中的 `~/` 前缀
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}
