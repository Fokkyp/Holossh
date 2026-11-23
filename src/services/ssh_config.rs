//! 本机 `~/.ssh/config` 只读解析服务（对应 07 引导页「导入 ~/.ssh/config」）
//!
//! 仅做只读解析，绝不修改用户原 `~/.ssh/config` 文件。

use crate::state::host::{AuthMethod, ConnectionKind, HostProfile, HostStatus};
use std::fs;
use std::path::PathBuf;

/// 返回本机默认 SSH 配置文件路径
pub fn default_ssh_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".ssh").join("config"))
}

/// 扫描并只读解析本机 `~/.ssh/config` 中的具体主机条目（忽略通配符 `Host *`）
pub fn load_local_ssh_config_hosts() -> Vec<HostProfile> {
    let Some(path) = default_ssh_config_path() else {
        return Vec::new();
    };
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_ssh_config(&content)
}

/// 解析标准 OpenSSH config 文本
pub fn parse_ssh_config(content: &str) -> Vec<HostProfile> {
    let mut hosts = Vec::new();
    let mut current: Option<HostProfile> = None;
    let mut pending_jumps: Vec<(usize, String)> = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        let value = parts.collect::<Vec<_>>().join(" ");
        if value.is_empty() {
            continue;
        }

        match key.to_ascii_lowercase().as_str() {
            "host" => {
                if let Some(prev) = current.take() {
                    if !prev.hostname.is_empty() {
                        hosts.push(prev);
                    }
                }
                let alias = value.split_whitespace().next().unwrap_or("");
                if !alias.is_empty() && !alias.contains('*') && !alias.contains('?') {
                    let default_user =
                        std::env::var("USER").unwrap_or_else(|_| "root".to_string());
                    let now_ms = crate::state::host::now_unix_ms();
                    current = Some(HostProfile {
                        id: crate::state::host::generate_host_uuid(),
                        name: alias.to_string(),
                        group: String::new(),
                        connection: ConnectionKind::Direct,
                        jump_host_id: None,
                        proxy_host: None,
                        proxy_port: None,
                        hostname: alias.to_string(),
                        port: 22,
                        username: default_user,
                        auth_method: AuthMethod::Auto,
                        private_key_path: None,
                        created_at_ms: now_ms,
                        updated_at_ms: now_ms,
                        status: HostStatus::Idle,
                    });
                }
            }
            "hostname" => {
                if let Some(host) = current.as_mut() {
                    host.hostname = value;
                }
            }
            "user" => {
                if let Some(host) = current.as_mut() {
                    host.username = value;
                }
            }
            "port" => {
                if let (Some(host), Ok(port)) = (current.as_mut(), value.parse::<u16>()) {
                    host.port = port;
                }
            }
            "identityfile" => {
                if let Some(host) = current.as_mut() {
                    host.private_key_path = Some(value);
                }
            }
            "proxyjump" => {
                if let Some(host) = current.as_mut() {
                    host.connection = ConnectionKind::JumpHost;
                    pending_jumps.push((hosts.len(), value));
                }
            }
            _ => {}
        }
    }

    if let Some(last) = current {
        if !last.hostname.is_empty() {
            hosts.push(last);
        }
    }

    // 将 ProxyJump 别名自动关联到已解析出的对应跳板主机 ID
    for (idx, jump_alias) in pending_jumps {
        let clean = jump_alias.split(',').next().unwrap_or("").trim();
        let target_id = hosts
            .iter()
            .find(|h| h.name == clean || h.hostname == clean)
            .map(|h| h.id.clone());
        if let Some(host) = hosts.get_mut(idx) {
            host.jump_host_id = target_id;
        }
    }

    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_ssh_config_entries() {
        let cfg = r#"
            # comment
            Host *
                ServerAliveInterval 60

            Host edge-01
                HostName 198.51.100.10
                User root
                Port 2222
                IdentityFile ~/.ssh/id_ed25519

            Host internal-db
                HostName 203.0.113.20
                User postgres
                ProxyJump edge-01
        "#;

        let hosts = parse_ssh_config(cfg);
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].name, "edge-01");
        assert_eq!(hosts[0].hostname, "198.51.100.10");
        assert_eq!(hosts[0].port, 2222);
        assert_eq!(
            hosts[0].private_key_path.as_deref(),
            Some("~/.ssh/id_ed25519")
        );
        assert_eq!(hosts[0].id.len(), 36);
        assert_eq!(hosts[1].connection, ConnectionKind::JumpHost);
        assert_eq!(
            hosts[1].jump_host_id.as_deref(),
            Some(hosts[0].id.as_str())
        );
    }
}
