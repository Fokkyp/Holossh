//! 纯 Rust 异步 SSHv2 与 SFTP 真实连接引擎（基于 `russh 0.65` + `russh-sftp 3.0` + `tokio`）
//!
//! 能力清单：
//! - 5 种身份验证方式：自动（私钥 → SSH Agent → 密钥库密码）、密码、私钥、SSH Agent、交互式
//! - 实时全双工 `xterm-256color` PTY 终端通道与窗口尺寸同步
//! - 同会话复用 SFTP 子系统（真实目录列表、创建文件夹、流式上传与下载进度回报）

use crate::i18n::{tr, trf};
use crate::services::ssh_keys::{discover_local_ssh_keys, expand_tilde};
use crate::services::sftp::{format_file_size, format_mtime, format_unix_permissions};
use crate::state::host::{AuthMethod, ConnectionKind, HostProfile};
use crate::state::sftp::{SftpEntry, TransferDirection};
use russh::client::{self, AuthResult, KeyboardInteractiveAuthResponse};
use russh::keys::{
    HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate, decode_secret_key, known_hosts,
    load_public_key, load_secret_key,
};
use russh::{ChannelMsg, Disconnect, MethodKind, MethodSet, Preferred};
use russh_sftp::client::{Config as SftpConfig, SftpSession};
use russh_sftp::protocol::OpenFlags;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

static TOKIO_RT: OnceLock<Runtime> = OnceLock::new();
static DOWNLOAD_PART_SEQ: AtomicU64 = AtomicU64::new(1);
static TRANSFER_ID_SEQ: AtomicU64 = AtomicU64::new(1);

pub(crate) fn runtime() -> &'static Runtime {
    TOKIO_RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .thread_name("holossh-io")
            .build()
            .expect("failed to build Holossh tokio runtime")
    })
}

/// UI 发送给单个 SSH 会话的控制命令
#[derive(Debug)]
pub enum SshCommand {
    Write(Vec<u8>),
    Resize { cols: u32, rows: u32 },
    /// 终端内交互式认证应答（2FA/OTP、键盘交互、未存密码或私钥口令；`None` 表示用户取消）
    AuthResponse { prompt_id: u64, value: Option<String> },
    /// 终端内交互式主机公钥指纹确认应答（`true` 信任并写入 known_hosts，`false` 拒绝断开）
    HostKeyResponse { prompt_id: u64, accepted: bool },
    SftpList { path: String },
    SftpCreateDir { parent_dir: String, folder_name: String },
    SftpDelete { path: String, is_dir: bool },
    SftpUpload {
        transfer_id: u64,
        local_path: PathBuf,
        remote_dir: String,
    },
    SftpDownload {
        transfer_id: u64,
        remote_path: String,
        local_path: PathBuf,
    },
    SftpCancelTransfer {
        transfer_id: u64,
    },
    Disconnect,
}

/// 后台 SSH/SFTP 任务回传给 UI 的事件流
#[derive(Debug, Clone)]
pub enum SshEvent {
    Connecting {
        pane_id: String,
        host_id: String,
        message: String,
    },
    /// 认证阶段向终端输出的状态提示（不改变连接态）
    AuthNotice {
        pane_id: String,
        message: String,
    },
    /// 认证阶段请求用户在终端内实时输入（密码/私钥口令/2FA 验证码）
    AuthPrompt {
        pane_id: String,
        prompt_id: u64,
        prompt: String,
        echo: bool,
    },
    /// 握手阶段请求用户在终端内确认未知主机指纹或处理主机指纹变更告警
    HostKeyPrompt {
        pane_id: String,
        prompt_id: u64,
        hostname: String,
        port: u16,
        key_type: String,
        fingerprint: String,
        is_key_changed: bool,
    },
    Connected {
        pane_id: String,
        host_id: String,
    },
    Output {
        pane_id: String,
        data: Vec<u8>,
    },
    Error {
        pane_id: String,
        host_id: String,
        message: String,
    },
    Disconnected {
        pane_id: String,
        host_id: String,
        reason: String,
        /// `true` 表示非远端正常 `exit`（如断网/休眠唤醒/保活超时），可触发智能自动重连
        unexpected: bool,
    },
    SftpListed {
        pane_id: String,
        path: String,
        entries: Vec<SftpEntry>,
    },
    SftpTransferProgress {
        pane_id: String,
        transfer_id: u64,
        direction: TransferDirection,
        file_name: String,
        transferred_mb: f32,
        total_mb: f32,
        progress: f32,
        done: bool,
    },
    SftpTransferCancelled {
        pane_id: String,
        transfer_id: u64,
    },
    SftpError {
        pane_id: String,
        message: String,
    },
}

/// 活跃会话句柄（用于从 GPUI 线程非阻塞投递按键、缩放与 SFTP 指令）
#[derive(Clone)]
pub struct PaneSessionHandle {
    pub tx: mpsc::UnboundedSender<SshCommand>,
}

impl PaneSessionHandle {
    pub fn send_bytes(&self, data: Vec<u8>) {
        let _ = self.tx.send(SshCommand::Write(data));
    }

    pub fn resize(&self, cols: u32, rows: u32) {
        let _ = self.tx.send(SshCommand::Resize { cols, rows });
    }

    pub fn send_auth_response(&self, prompt_id: u64, value: Option<String>) {
        let _ = self.tx.send(SshCommand::AuthResponse { prompt_id, value });
    }

    pub fn send_host_key_response(&self, prompt_id: u64, accepted: bool) {
        let _ = self.tx.send(SshCommand::HostKeyResponse { prompt_id, accepted });
    }

    pub fn sftp_list(&self, path: String) {
        let _ = self.tx.send(SshCommand::SftpList { path });
    }

    pub fn sftp_create_dir(&self, parent_dir: String, folder_name: String) {
        let _ = self.tx.send(SshCommand::SftpCreateDir {
            parent_dir,
            folder_name,
        });
    }

    pub fn sftp_delete(&self, path: String, is_dir: bool) {
        let _ = self.tx.send(SshCommand::SftpDelete { path, is_dir });
    }

    pub fn sftp_upload(&self, local_path: PathBuf, remote_dir: String) -> u64 {
        let transfer_id = TRANSFER_ID_SEQ.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(SshCommand::SftpUpload {
            transfer_id,
            local_path,
            remote_dir,
        });
        transfer_id
    }

    pub fn sftp_download(&self, remote_path: String, local_path: PathBuf) -> u64 {
        let transfer_id = TRANSFER_ID_SEQ.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(SshCommand::SftpDownload {
            transfer_id,
            remote_path,
            local_path,
        });
        transfer_id
    }

    pub fn sftp_cancel_transfer(&self, transfer_id: u64) {
        let _ = self.tx.send(SshCommand::SftpCancelTransfer { transfer_id });
    }

    pub fn disconnect(&self) {
        let _ = self.tx.send(SshCommand::Disconnect);
    }
}

/// 握手阶段主机公钥指纹确认桥接器（在 `check_server_key` 与 `cmd_rx` 之间安全传递 yes/no）
#[derive(Clone)]
struct HostKeyConfirmBridge {
    pane_id: String,
    event_tx: mpsc::Sender<SshEvent>,
    pending: Arc<StdMutex<HashMap<u64, tokio::sync::oneshot::Sender<bool>>>>,
}

impl HostKeyConfirmBridge {
    fn new(pane_id: String, event_tx: mpsc::Sender<SshEvent>) -> Self {
        Self {
            pane_id,
            event_tx,
            pending: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    fn resolve(&self, prompt_id: u64, accepted: bool) {
        if let Ok(mut map) = self.pending.lock() {
            if let Some(tx) = map.remove(&prompt_id) {
                let _ = tx.send(accepted);
            }
        }
    }

    fn cancel_all(&self) {
        if let Ok(mut map) = self.pending.lock() {
            for (_, tx) in map.drain() {
                let _ = tx.send(false);
            }
        }
    }

    async fn ask_user(
        &self,
        hostname: &str,
        port: u16,
        key_type: String,
        fingerprint: String,
        is_key_changed: bool,
    ) -> bool {
        let prompt_id = NEXT_AUTH_PROMPT_ID.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        if let Ok(mut map) = self.pending.lock() {
            map.insert(prompt_id, tx);
        } else {
            return false;
        }
        if self
            .event_tx
            .send(SshEvent::HostKeyPrompt {
                pane_id: self.pane_id.clone(),
                prompt_id,
                hostname: hostname.to_string(),
                port,
                key_type,
                fingerprint,
                is_key_changed,
            })
            .await
            .is_err()
        {
            if let Ok(mut map) = self.pending.lock() {
                map.remove(&prompt_id);
            }
            return false;
        }
        let res = tokio::time::timeout(Duration::from_secs(120), rx)
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(false);
        if let Ok(mut map) = self.pending.lock() {
            map.remove(&prompt_id);
        }
        res
    }
}

/// 当用户明确确认覆盖变更后的主机公钥时，从 `~/.ssh/known_hosts` 中安全移除该主机的旧条目
fn remove_known_host_entries(hostname: &str, port: u16) {
    let Some(path) = dirs::home_dir().map(|h| h.join(".ssh").join("known_hosts")) else {
        return;
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };
    let target_bracketed = format!("[{hostname}]:{port}");
    let mut kept = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            kept.push(line.to_string());
            continue;
        }
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let host_field = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("");
        let tokens: Vec<&str> = host_field
            .split(',')
            .filter(|tok| {
                let t = tok.trim();
                if port == 22 {
                    t != hostname && t != target_bracketed
                } else {
                    t != target_bracketed
                }
            })
            .collect();
        if !tokens.is_empty() {
            if tokens.len() == host_field.split(',').count() {
                kept.push(line.to_string());
            } else {
                kept.push(format!("{} {}", tokens.join(","), rest.trim_start()));
            }
        }
    }
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    let _ = std::fs::write(&path, out);
}

struct HoloClientHandler {
    hostname: String,
    port: u16,
    auto_accept_host_key: bool,
    confirm_bridge: Option<HostKeyConfirmBridge>,
}

impl HoloClientHandler {
    fn new(
        hostname: impl Into<String>,
        port: u16,
        auto_accept_host_key: bool,
        confirm_bridge: Option<HostKeyConfirmBridge>,
    ) -> Self {
        Self {
            hostname: hostname.into(),
            port,
            auto_accept_host_key,
            confirm_bridge,
        }
    }
}

impl client::Handler for HoloClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let pubkey = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(cert) => {
                russh::keys::PublicKey::from(cert.public_key().clone())
            }
        };

        let is_key_changed = match known_hosts::check_known_hosts(&self.hostname, self.port, &pubkey)
        {
            Ok(true) => return Ok(true),
            Err(russh::keys::Error::KeyChanged { .. }) => true,
            Ok(false) | Err(_) => {
                if self.auto_accept_host_key {
                    if let Some(ssh_dir) = dirs::home_dir().map(|h| h.join(".ssh")) {
                        let _ = std::fs::create_dir_all(&ssh_dir);
                    }
                    let _ = known_hosts::learn_known_hosts(&self.hostname, self.port, &pubkey);
                    return Ok(true);
                }
                false
            }
        };

        // 未知主机且未开启自动信任，或主机指纹发生变更（潜在 MITM 风险）→ 终端内交互确认
        let Some(bridge) = self.confirm_bridge.as_ref() else {
            return Ok(false);
        };
        let key_type = pubkey.algorithm().to_string();
        let fingerprint = pubkey.fingerprint(HashAlg::Sha256).to_string();
        let accepted = bridge
            .ask_user(&self.hostname, self.port, key_type, fingerprint, is_key_changed)
            .await;
        if accepted {
            if let Some(ssh_dir) = dirs::home_dir().map(|h| h.join(".ssh")) {
                let _ = std::fs::create_dir_all(&ssh_dir);
            }
            if is_key_changed {
                remove_known_host_entries(&self.hostname, self.port);
            }
            let _ = known_hosts::learn_known_hosts(&self.hostname, self.port, &pubkey);
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// 已解析的单个跳板机节点（携带着已保存主机的完整配置及从密钥库解密出的凭据）
#[derive(Debug, Clone)]
pub struct ResolvedJumpHop {
    pub host: HostProfile,
    pub password: Option<String>,
    pub key_pem: Option<String>,
}

/// 启动一个真实异步 SSHv2 PTY 会话并立即返回命令句柄
#[allow(clippy::too_many_arguments)]
pub fn spawn_ssh_session(
    pane_id: String,
    host: HostProfile,
    vault_password: Option<String>,
    vault_key_pem: Option<String>,
    jump_hops: Vec<ResolvedJumpHop>,
    extra_key_paths: Vec<String>,
    cols: u32,
    rows: u32,
    keepalive_secs: u64,
    connect_timeout_secs: u64,
    auto_accept_host_key: bool,
    event_tx: mpsc::Sender<SshEvent>,
) -> PaneSessionHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SshCommand>();
    let handle = PaneSessionHandle { tx: cmd_tx };

    runtime().spawn(async move {
        run_session_loop(
            pane_id,
            host,
            vault_password,
            vault_key_pem,
            jump_hops,
            extra_key_paths,
            cols,
            rows,
            keepalive_secs,
            connect_timeout_secs,
            auto_accept_host_key,
            cmd_rx,
            event_tx,
        )
        .await;
    });

    handle
}

#[allow(clippy::too_many_arguments)]
async fn run_session_loop(
    pane_id: String,
    host: HostProfile,
    vault_password: Option<String>,
    vault_key_pem: Option<String>,
    jump_hops: Vec<ResolvedJumpHop>,
    extra_key_paths: Vec<String>,
    cols: u32,
    rows: u32,
    keepalive_secs: u64,
    connect_timeout_secs: u64,
    auto_accept_host_key: bool,
    mut cmd_rx: mpsc::UnboundedReceiver<SshCommand>,
    event_tx: mpsc::Sender<SshEvent>,
) {
    let host_id = host.id.clone();
    let timeout_secs = connect_timeout_secs.clamp(3, 60);
    let via_desc = match host.connection {
        ConnectionKind::Direct | ConnectionKind::Local => String::new(),
        ConnectionKind::JumpHost => {
            let label = if !jump_hops.is_empty() {
                jump_hops
                    .iter()
                    .map(|h| h.host.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" → ")
            } else {
                tr("common.unconfigured").to_string()
            };
            trf("svc.ssh.via_jump", &[("addr", &label)])
        }
        ConnectionKind::Socks5 => {
            let label = host
                .effective_proxy_host_port()
                .map(|(h, p)| format!("{h}:{p}"))
                .unwrap_or_else(|| tr("common.unconfigured").to_string());
            trf("svc.ssh.via_socks5", &[("addr", &label)])
        }
        ConnectionKind::HttpProxy => {
            let label = host
                .effective_proxy_host_port()
                .map(|(h, p)| format!("{h}:{p}"))
                .unwrap_or_else(|| tr("common.unconfigured").to_string());
            trf("svc.ssh.via_http", &[("addr", &label)])
        }
    };
    let _ = event_tx
        .send(SshEvent::Connecting {
            pane_id: pane_id.clone(),
            host_id: host_id.clone(),
            message: trf(
                "svc.ssh.connecting",
                &[
                    ("user", &host.username),
                    ("host", &host.hostname),
                    ("port", &host.port.to_string()),
                    ("via", &via_desc),
                ],
            ),
        })
        .await;

    let preferred = Preferred {
        // 默认禁用 SSH 压缩，彻底规避 russh 在 zlib@openssh.com 下 SFTP 大文件上传异常（Tabby #10780）
        compression: Cow::Borrowed(&[russh::compression::NONE]),
        ..Preferred::default()
    };
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(600)),
        keepalive_interval: if keepalive_secs > 0 {
            Some(Duration::from_secs(keepalive_secs))
        } else {
            None
        },
        keepalive_max: 3,
        window_size: 4 * 1024 * 1024,
        maximum_packet_size: 32_768,
        channel_buffer_size: 256,
        preferred,
        // 开启 TCP_NODELAY 消除 Nagle 40ms 延迟，确保终端按键输入零迟滞
        nodelay: true,
        ..Default::default()
    });

    // 保留全部跳板机链会话存活，防止底层 direct-tcpip 隧道提前断开；
    // 握手期间并发监听 cmd_rx，以支持交互式主机指纹确认（HostKeyResponse）与随时取消（Disconnect）
    let confirm_bridge = HostKeyConfirmBridge::new(pane_id.clone(), event_tx.clone());
    let _jump_guards: Vec<client::Handle<HoloClientHandler>>;
    let mut session = {
        let transport_fut = establish_ssh_transport(
            config,
            &host,
            &jump_hops,
            &extra_key_paths,
            timeout_secs,
            auto_accept_host_key,
            confirm_bridge.clone(),
        );
        tokio::pin!(transport_fut);
        let outcome = loop {
            tokio::select! {
                res = &mut transport_fut => break res,
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(SshCommand::HostKeyResponse { prompt_id, accepted }) => {
                            confirm_bridge.resolve(prompt_id, accepted);
                        }
                        Some(SshCommand::Disconnect) | None => {
                            confirm_bridge.cancel_all();
                            return;
                        }
                        _ => {}
                    }
                }
            }
        };
        match outcome {
            Ok((s, jumps)) => {
                _jump_guards = jumps;
                s
            }
            Err(err_msg) => {
                let _ = event_tx
                    .send(SshEvent::Error {
                        pane_id,
                        host_id,
                        message: err_msg,
                    })
                    .await;
                return;
            }
        }
    };

    // 仅使用本次经密钥库验证后显式传入的密码与私钥（绝不私自绕过密钥库策略读取）
    let password = vault_password.filter(|p| !p.is_empty());
    let key_pem = vault_key_pem.filter(|p| !p.trim().is_empty());

    // 执行 RFC 4252 协商式 5 种身份验证（支持 2FA 链式认证、终端内交互提示与私钥口令输入）
    let mut auth_io = TerminalAuthIo {
        pane_id: &pane_id,
        cmd_rx: &mut cmd_rx,
        event_tx: &event_tx,
    };
    let auth_ok = authenticate_session(
        &mut session,
        &host,
        password.as_deref(),
        key_pem.as_deref(),
        &extra_key_paths,
        Some(&mut auth_io),
    )
    .await;
    match auth_ok {
        Ok(true) => {}
        Ok(false) => {
            let _ = event_tx
                .send(SshEvent::Error {
                    pane_id,
                    host_id,
                    message: trf(
                        "svc.ssh.auth_rejected",
                        &[("method", host.auth_method.label_zh())],
                    ),
                })
                .await;
            return;
        }
        Err(msg) => {
            let _ = event_tx
                .send(SshEvent::Error {
                    pane_id,
                    host_id,
                    message: trf("svc.ssh.auth_err", &[("msg", &msg)]),
                })
                .await;
            return;
        }
    }

    // 打开 PTY Shell 通道
    let mut channel = match session.channel_open_session().await {
        Ok(c) => c,
        Err(e) => {
            let _ = event_tx
                .send(SshEvent::Error {
                    pane_id,
                    host_id,
                    message: trf("svc.ssh.channel_err", &[("err", &e.to_string())]),
                })
                .await;
            return;
        }
    };

    if let Err(e) = channel
        .request_pty(true, "xterm-256color", cols, rows, 0, 0, &[])
        .await
    {
        let _ = event_tx
            .send(SshEvent::Error {
                pane_id,
                host_id,
                message: trf("svc.ssh.pty_err", &[("err", &e.to_string())]),
            })
            .await;
        return;
    }

    if let Err(e) = channel.request_shell(true).await {
        let _ = event_tx
            .send(SshEvent::Error {
                pane_id,
                host_id,
                message: trf("svc.ssh.shell_err", &[("err", &e.to_string())]),
            })
            .await;
        return;
    }

    let _ = event_tx
        .send(SshEvent::Connected {
            pane_id: pane_id.clone(),
            host_id: host_id.clone(),
        })
        .await;

    let session = Arc::new(session);
    let sftp_slot: Arc<AsyncMutex<Option<Arc<SftpSession>>>> = Arc::new(AsyncMutex::new(None));
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let transfer_cancels: Arc<StdMutex<HashMap<u64, Arc<AtomicBool>>>> =
        Arc::new(StdMutex::new(HashMap::new()));
    // 背压暂存槽：当 UI 消费不及导致有界通道满时暂停 channel.wait()，同时 cmd_rx 永不阻塞（保障 Ctrl+C 瞬时响应）
    let mut pending_output: Option<Vec<u8>> = None;
    let mut saw_exit_status = false;

    loop {
        tokio::select! {
            biased;
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(SshCommand::Write(bytes)) => {
                        if let Err(e) = channel.data(&bytes[..]).await {
                            cancel_flag.store(true, Ordering::Relaxed);
                            let _ = event_tx
                                .send(SshEvent::Disconnected {
                                    pane_id: pane_id.clone(),
                                    host_id: host_id.clone(),
                                    reason: trf("svc.ssh.write_err", &[("err", &e.to_string())]),
                                    unexpected: true,
                                })
                                .await;
                            break;
                        }
                    }
                    Some(SshCommand::Resize { cols, rows }) => {
                        let _ = channel.window_change(cols, rows, 0, 0).await;
                    }
                    // 所有 SFTP 请求在独立 tokio 任务中异步并发执行，绝不阻塞 PTY 终端读写循环（根治 Tabby #10685/#8972）
                    Some(SshCommand::SftpList { path }) => {
                        let session_c = Arc::clone(&session);
                        let sftp_slot_c = Arc::clone(&sftp_slot);
                        let pane_c = pane_id.clone();
                        let tx_c = event_tx.clone();
                        tokio::spawn(async move {
                            let sftp = match ensure_sftp(&session_c, &sftp_slot_c).await {
                                Ok(s) => s,
                                Err(err) => {
                                    let _ = tx_c.send(SshEvent::SftpError { pane_id: pane_c, message: err }).await;
                                    return;
                                }
                            };
                            match list_remote_dir(&sftp, &path).await {
                                Ok((resolved_path, entries)) => {
                                    let _ = tx_c
                                        .send(SshEvent::SftpListed {
                                            pane_id: pane_c,
                                            path: resolved_path,
                                            entries,
                                        })
                                        .await;
                                }
                                Err(err) => {
                                    let _ = tx_c.send(SshEvent::SftpError { pane_id: pane_c, message: err }).await;
                                }
                            }
                        });
                    }
                    Some(SshCommand::SftpCreateDir { parent_dir, folder_name }) => {
                        let session_c = Arc::clone(&session);
                        let sftp_slot_c = Arc::clone(&sftp_slot);
                        let pane_c = pane_id.clone();
                        let tx_c = event_tx.clone();
                        tokio::spawn(async move {
                            let sftp = match ensure_sftp(&session_c, &sftp_slot_c).await {
                                Ok(s) => s,
                                Err(err) => {
                                    let _ = tx_c.send(SshEvent::SftpError { pane_id: pane_c, message: err }).await;
                                    return;
                                }
                            };
                            let base_dir = resolve_remote_dir(&sftp, &parent_dir).await;
                            let clean_name = folder_name.trim().trim_matches('/');
                            if clean_name.is_empty() {
                                return;
                            }
                            let full_path = if base_dir == "/" {
                                format!("/{clean_name}")
                            } else {
                                format!("{}/{clean_name}", base_dir.trim_end_matches('/'))
                            };
                            if let Err(e) = sftp.create_dir(&full_path).await {
                                let _ = tx_c
                                    .send(SshEvent::SftpError {
                                        pane_id: pane_c,
                                        message: trf("svc.ssh.mkdir_err", &[("err", &e.to_string())]),
                                    })
                                    .await;
                            } else if let Ok((resolved, entries)) = list_remote_dir(&sftp, &base_dir).await {
                                let _ = tx_c
                                    .send(SshEvent::SftpListed {
                                        pane_id: pane_c,
                                        path: resolved,
                                        entries,
                                    })
                                    .await;
                            }
                        });
                    }
                    Some(SshCommand::SftpDelete { path, is_dir }) => {
                        let session_c = Arc::clone(&session);
                        let sftp_slot_c = Arc::clone(&sftp_slot);
                        let pane_c = pane_id.clone();
                        let tx_c = event_tx.clone();
                        tokio::spawn(async move {
                            let sftp = match ensure_sftp(&session_c, &sftp_slot_c).await {
                                Ok(s) => s,
                                Err(err) => {
                                    let _ = tx_c.send(SshEvent::SftpError { pane_id: pane_c, message: err }).await;
                                    return;
                                }
                            };
                            let res = if is_dir {
                                sftp.remove_dir(&path).await
                            } else {
                                sftp.remove_file(&path).await
                            };
                            if let Err(e) = res {
                                let _ = tx_c
                                    .send(SshEvent::SftpError {
                                        pane_id: pane_c,
                                        message: trf("svc.ssh.delete_err", &[("err", &e.to_string())]),
                                    })
                                    .await;
                            } else if let Some(parent) = parent_remote_dir(&path) {
                                if let Ok((resolved, entries)) = list_remote_dir(&sftp, &parent).await {
                                    let _ = tx_c
                                        .send(SshEvent::SftpListed {
                                            pane_id: pane_c,
                                            path: resolved,
                                            entries,
                                        })
                                        .await;
                                }
                            }
                        });
                    }
                    Some(SshCommand::SftpUpload {
                        transfer_id,
                        local_path,
                        remote_dir,
                    }) => {
                        let session_c = Arc::clone(&session);
                        let sftp_slot_c = Arc::clone(&sftp_slot);
                        let pane_c = pane_id.clone();
                        let tx_c = event_tx.clone();
                        let cancel_c = Arc::clone(&cancel_flag);
                        let task_cancel = Arc::new(AtomicBool::new(false));
                        if let Ok(mut map) = transfer_cancels.lock() {
                            map.insert(transfer_id, Arc::clone(&task_cancel));
                        }
                        let cancels_map = Arc::clone(&transfer_cancels);
                        tokio::spawn(async move {
                            let sftp = match ensure_sftp(&session_c, &sftp_slot_c).await {
                                Ok(s) => s,
                                Err(err) => {
                                    if let Ok(mut map) = cancels_map.lock() {
                                        map.remove(&transfer_id);
                                    }
                                    let _ = tx_c
                                        .send(SshEvent::SftpTransferCancelled {
                                            pane_id: pane_c.clone(),
                                            transfer_id,
                                        })
                                        .await;
                                    let _ = tx_c
                                        .send(SshEvent::SftpError {
                                            pane_id: pane_c,
                                            message: err,
                                        })
                                        .await;
                                    return;
                                }
                            };
                            let resolved_dir = resolve_remote_dir(&sftp, &remote_dir).await;
                            let res = upload_via_sftp(
                                &sftp,
                                &pane_c,
                                transfer_id,
                                &local_path,
                                &resolved_dir,
                                &tx_c,
                                &cancel_c,
                                &task_cancel,
                            )
                            .await;
                            if let Ok(mut map) = cancels_map.lock() {
                                map.remove(&transfer_id);
                            }
                            match res {
                                Ok(()) => {
                                    if let Ok((resolved, entries)) =
                                        list_remote_dir(&sftp, &resolved_dir).await
                                    {
                                        let _ = tx_c
                                            .send(SshEvent::SftpListed {
                                                pane_id: pane_c,
                                                path: resolved,
                                                entries,
                                            })
                                            .await;
                                    }
                                }
                                Err(err) if task_cancel.load(Ordering::Relaxed) => {
                                    let _ = err;
                                    let _ = tx_c
                                        .send(SshEvent::SftpTransferCancelled {
                                            pane_id: pane_c,
                                            transfer_id,
                                        })
                                        .await;
                                }
                                Err(err) => {
                                    let _ = tx_c
                                        .send(SshEvent::SftpTransferCancelled {
                                            pane_id: pane_c.clone(),
                                            transfer_id,
                                        })
                                        .await;
                                    let _ = tx_c
                                        .send(SshEvent::SftpError {
                                            pane_id: pane_c,
                                            message: err,
                                        })
                                        .await;
                                }
                            }
                        });
                    }
                    Some(SshCommand::SftpDownload {
                        transfer_id,
                        remote_path,
                        local_path,
                    }) => {
                        let session_c = Arc::clone(&session);
                        let sftp_slot_c = Arc::clone(&sftp_slot);
                        let pane_c = pane_id.clone();
                        let tx_c = event_tx.clone();
                        let cancel_c = Arc::clone(&cancel_flag);
                        let task_cancel = Arc::new(AtomicBool::new(false));
                        if let Ok(mut map) = transfer_cancels.lock() {
                            map.insert(transfer_id, Arc::clone(&task_cancel));
                        }
                        let cancels_map = Arc::clone(&transfer_cancels);
                        tokio::spawn(async move {
                            let sftp = match ensure_sftp(&session_c, &sftp_slot_c).await {
                                Ok(s) => s,
                                Err(err) => {
                                    if let Ok(mut map) = cancels_map.lock() {
                                        map.remove(&transfer_id);
                                    }
                                    let _ = tx_c
                                        .send(SshEvent::SftpTransferCancelled {
                                            pane_id: pane_c.clone(),
                                            transfer_id,
                                        })
                                        .await;
                                    let _ = tx_c
                                        .send(SshEvent::SftpError {
                                            pane_id: pane_c,
                                            message: err,
                                        })
                                        .await;
                                    return;
                                }
                            };
                            let res = download_via_sftp(
                                &sftp,
                                &pane_c,
                                transfer_id,
                                &remote_path,
                                &local_path,
                                &tx_c,
                                &cancel_c,
                                &task_cancel,
                            )
                            .await;
                            if let Ok(mut map) = cancels_map.lock() {
                                map.remove(&transfer_id);
                            }
                            match res {
                                Ok(()) => {}
                                Err(_) if task_cancel.load(Ordering::Relaxed) => {
                                    let _ = tx_c
                                        .send(SshEvent::SftpTransferCancelled {
                                            pane_id: pane_c,
                                            transfer_id,
                                        })
                                        .await;
                                }
                                Err(err) => {
                                    let _ = tx_c
                                        .send(SshEvent::SftpTransferCancelled {
                                            pane_id: pane_c.clone(),
                                            transfer_id,
                                        })
                                        .await;
                                    let _ = tx_c
                                        .send(SshEvent::SftpError {
                                            pane_id: pane_c,
                                            message: err,
                                        })
                                        .await;
                                }
                            }
                        });
                    }
                    Some(SshCommand::SftpCancelTransfer { transfer_id }) => {
                        if let Ok(map) = transfer_cancels.lock() {
                            if let Some(flag) = map.get(&transfer_id) {
                                flag.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                    Some(SshCommand::AuthResponse { .. })
                    | Some(SshCommand::HostKeyResponse { .. }) => {
                        // 会话已认证进入 PTY 阶段，忽略迟到的握手/认证应答
                    }
                    Some(SshCommand::Disconnect) | None => {
                        cancel_flag.store(true, Ordering::Relaxed);
                        let _ = channel.eof().await;
                        let _ = session.disconnect(Disconnect::ByApplication, "", "").await;
                        break;
                    }
                }
            }
            permit = event_tx.reserve(), if pending_output.is_some() => {
                match permit {
                    Ok(permit) => {
                        if let Some(data) = pending_output.take() {
                            permit.send(SshEvent::Output {
                                pane_id: pane_id.clone(),
                                data,
                            });
                        }
                    }
                    Err(_) => break,
                }
            }
            msg = channel.wait(), if pending_output.is_none() => {
                match msg {
                    Some(ChannelMsg::Data { data })
                    | Some(ChannelMsg::ExtendedData { data, .. }) => {
                        let bytes = data.to_vec();
                        if let Err(mpsc::error::TrySendError::Full(SshEvent::Output {
                            data: back,
                            ..
                        })) = event_tx.try_send(SshEvent::Output {
                            pane_id: pane_id.clone(),
                            data: bytes,
                        }) {
                            pending_output = Some(back);
                        }
                    }
                    Some(ChannelMsg::ExitStatus { .. }) => {
                        saw_exit_status = true;
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                        if let Some(data) = pending_output.take() {
                            let _ = event_tx
                                .send(SshEvent::Output {
                                    pane_id: pane_id.clone(),
                                    data,
                                })
                                .await;
                        }
                        cancel_flag.store(true, Ordering::Relaxed);
                        let _ = event_tx
                            .send(SshEvent::Disconnected {
                                pane_id: pane_id.clone(),
                                host_id: host_id.clone(),
                                reason: tr("svc.ssh.closed").into(),
                                unexpected: !saw_exit_status,
                            })
                            .await;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}

static NEXT_AUTH_PROMPT_ID: AtomicU64 = AtomicU64::new(1);

/// 认证阶段的终端交互桥梁（用于在终端内输出提示并实时接收密码/私钥口令/2FA OTP）
struct TerminalAuthIo<'a> {
    pane_id: &'a str,
    cmd_rx: &'a mut mpsc::UnboundedReceiver<SshCommand>,
    event_tx: &'a mpsc::Sender<SshEvent>,
}

impl<'a> TerminalAuthIo<'a> {
    fn notice(&self, msg: impl Into<String>) {
        let _ = self.event_tx.try_send(SshEvent::AuthNotice {
            pane_id: self.pane_id.to_string(),
            message: msg.into(),
        });
    }

    async fn prompt(&mut self, prompt: impl Into<String>, echo: bool) -> Option<String> {
        let prompt_id = NEXT_AUTH_PROMPT_ID.fetch_add(1, Ordering::Relaxed);
        let _ = self
            .event_tx
            .send(SshEvent::AuthPrompt {
                pane_id: self.pane_id.to_string(),
                prompt_id,
                prompt: prompt.into(),
                echo,
            })
            .await;
        while let Some(cmd) = self.cmd_rx.recv().await {
            match cmd {
                SshCommand::AuthResponse {
                    prompt_id: pid,
                    value,
                } if pid == prompt_id => return value,
                SshCommand::Disconnect => return None,
                _ => {}
            }
        }
        None
    }
}

/// RFC 4252 协商式认证步骤（参考 Tabby `authMethodSelection.ts` 队列规划模型）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthStep {
    Agent,
    PublicKeys,
    SavedPassword,
    KeyboardInteractive,
    PromptPassword,
}

impl AuthStep {
    fn method_kind(self) -> MethodKind {
        match self {
            Self::Agent | Self::PublicKeys => MethodKind::PublicKey,
            Self::SavedPassword | Self::PromptPassword => MethodKind::Password,
            Self::KeyboardInteractive => MethodKind::KeyboardInteractive,
        }
    }
}

fn build_initial_auth_queue(method: AuthMethod, has_saved_password: bool) -> Vec<AuthStep> {
    match method {
        // Auto 顺序对齐 Tabby：优先已解锁的 Agent，再试私钥，再试已存密码，再试交互式，最后终端输入密码
        AuthMethod::Auto => {
            let mut q = vec![AuthStep::Agent, AuthStep::PublicKeys];
            if has_saved_password {
                q.push(AuthStep::SavedPassword);
            }
            q.push(AuthStep::KeyboardInteractive);
            q.push(AuthStep::PromptPassword);
            q
        }
        AuthMethod::Agent => vec![AuthStep::Agent],
        AuthMethod::PrivateKey => vec![AuthStep::PublicKeys],
        AuthMethod::Password => {
            let mut q = Vec::new();
            if has_saved_password {
                q.push(AuthStep::SavedPassword);
            }
            q.push(AuthStep::PromptPassword);
            q
        }
        AuthMethod::KeyboardInteractive => vec![AuthStep::KeyboardInteractive],
    }
}

fn select_next_auth_step(queue: &[AuthStep], allowed: &[MethodKind]) -> Option<usize> {
    if allowed.is_empty() {
        return (!queue.is_empty()).then_some(0);
    }
    queue
        .iter()
        .position(|step| allowed.contains(&step.method_kind()))
}

fn update_auth_plan_after_failure(
    queue: &mut Vec<AuthStep>,
    allowed: &mut Vec<MethodKind>,
    remaining: &MethodSet,
    partial_success: bool,
    has_saved_password: bool,
) {
    if !remaining.is_empty() {
        *allowed = remaining.iter().copied().collect();
    }
    // RFC 4252 多因子（2FA/MFA）链式认证：第一因子成功后自动动态追加服务端要求的第二因子
    if partial_success {
        for &kind in allowed.iter() {
            let already_planned = queue.iter().any(|s| s.method_kind() == kind);
            if !already_planned {
                match kind {
                    MethodKind::KeyboardInteractive => queue.push(AuthStep::KeyboardInteractive),
                    MethodKind::Password => {
                        if has_saved_password {
                            queue.push(AuthStep::SavedPassword);
                        }
                        queue.push(AuthStep::PromptPassword);
                    }
                    MethodKind::PublicKey => {
                        queue.push(AuthStep::Agent);
                        queue.push(AuthStep::PublicKeys);
                    }
                    _ => {}
                }
            }
        }
    }
}

fn is_password_prompt_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("password") || lower.contains("passphrase") || lower.contains("密码")
}

async fn authenticate_session(
    session: &mut client::Handle<HoloClientHandler>,
    host: &HostProfile,
    password: Option<&str>,
    vault_key_pem: Option<&str>,
    extra_key_paths: &[String],
    mut auth_io: Option<&mut TerminalAuthIo<'_>>,
) -> Result<bool, String> {
    // 1. 先按 RFC 4252 发送 "none" 探测服务端实际允许的认证方式列表（如 PasswordAuthentication no 时绝不白试）
    let mut allowed: Vec<MethodKind> = Vec::new();
    match session.authenticate_none(&host.username).await {
        Ok(AuthResult::Success) => return Ok(true),
        Ok(AuthResult::Failure {
            remaining_methods, ..
        }) => {
            allowed = remaining_methods.iter().copied().collect();
        }
        Err(_) => {}
    }

    let has_saved_pwd = password.is_some_and(|p| !p.is_empty());
    let mut queue = build_initial_auth_queue(host.auth_method, has_saved_pwd);
    let mut pk_attempts: usize = 0;

    while let Some(idx) = select_next_auth_step(&queue, &allowed) {
        let step = queue.remove(idx);
        let outcome = match step {
            AuthStep::Agent => {
                if let Some(io) = auth_io.as_deref() {
                    if std::env::var("SSH_AUTH_SOCK")
                        .ok()
                        .filter(|s| !s.trim().is_empty())
                        .is_some()
                    {
                        io.notice(tr("svc.ssh.auth_trying_agent"));
                    }
                }
                try_ssh_agent(session, host, &mut pk_attempts).await
            }
            AuthStep::PublicKeys => {
                try_private_keys(
                    session,
                    host,
                    password,
                    vault_key_pem,
                    extra_key_paths,
                    auth_io.as_deref_mut(),
                    &mut pk_attempts,
                )
                .await
            }
            AuthStep::SavedPassword => {
                if let Some(pwd) = password {
                    session
                        .authenticate_password(&host.username, pwd)
                        .await
                        .map_err(|e| e.to_string())
                } else {
                    continue;
                }
            }
            AuthStep::PromptPassword => {
                let Some(io) = auth_io.as_deref_mut() else {
                    // 跳板机等无交互通道场景直接跳过终端询问
                    continue;
                };
                let mut final_res = AuthResult::Failure {
                    remaining_methods: MethodSet::empty(),
                    partial_success: false,
                };
                for _ in 0..3 {
                    let prompt = trf(
                        "svc.ssh.auth_prompt_pwd",
                        &[("user", &host.username), ("host", &host.hostname)],
                    );
                    let Some(entered) = io.prompt(prompt, false).await else {
                        return Ok(false);
                    };
                    match session.authenticate_password(&host.username, entered).await {
                        Ok(AuthResult::Success) => return Ok(true),
                        Ok(
                            fail @ AuthResult::Failure {
                                partial_success: true,
                                ..
                            },
                        ) => {
                            final_res = fail;
                            break;
                        }
                        Ok(fail) => {
                            final_res = fail;
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                }
                Ok(final_res)
            }
            AuthStep::KeyboardInteractive => {
                try_keyboard_interactive(
                    session,
                    &host.username,
                    password,
                    auth_io.as_deref_mut(),
                )
                .await
            }
        };

        match outcome {
            Ok(AuthResult::Success) => return Ok(true),
            Ok(AuthResult::Failure {
                remaining_methods,
                partial_success,
            }) => {
                if partial_success {
                    if let Some(io) = auth_io.as_deref() {
                        io.notice(tr("svc.ssh.auth_partial_2fa"));
                    }
                }
                update_auth_plan_after_failure(
                    &mut queue,
                    &mut allowed,
                    &remaining_methods,
                    partial_success,
                    has_saved_pwd,
                );
            }
            Err(err) => {
                if host.auth_method == AuthMethod::Auto && pk_attempts >= 3 {
                    if let Some(io) = auth_io.as_deref() {
                        io.notice(tr("svc.ssh.auth_too_many_keys"));
                    }
                }
                return Err(err);
            }
        }
    }

    if host.auth_method == AuthMethod::Auto && pk_attempts >= 3 {
        if let Some(io) = auth_io.as_deref() {
            io.notice(tr("svc.ssh.auth_too_many_keys"));
        }
    }
    Ok(false)
}

async fn try_private_keys(
    session: &mut client::Handle<HoloClientHandler>,
    host: &HostProfile,
    passphrase: Option<&str>,
    vault_key_pem: Option<&str>,
    extra_key_paths: &[String],
    mut auth_io: Option<&mut TerminalAuthIo<'_>>,
    pk_attempts: &mut usize,
) -> Result<AuthResult, String> {
    let hash_alg = session
        .best_supported_rsa_hash()
        .await
        .ok()
        .flatten()
        .flatten();

    let mut last_failure = AuthResult::Failure {
        remaining_methods: MethodSet::empty(),
        partial_success: false,
    };

    // 1. 优先使用密钥库解密出的内置私钥 PEM（跨设备同步后无需依赖本机文件）
    if let Some(pem) = vault_key_pem {
        let mut decoded =
            decode_secret_key(pem, None).or_else(|_| decode_secret_key(pem, passphrase));
        if decoded.is_err() {
            if let Some(io) = auth_io.as_deref_mut() {
                let prompt = trf("svc.ssh.auth_prompt_passphrase", &[("name", "vault")]);
                if let Some(entered) = io.prompt(prompt, false).await {
                    decoded = decode_secret_key(pem, Some(entered.as_str()));
                }
            }
        }
        if let Ok(secret_key) = decoded {
            *pk_attempts += 1;
            let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(secret_key), hash_alg);
            match session
                .authenticate_publickey(&host.username, key_with_hash)
                .await
            {
                Ok(AuthResult::Success) => return Ok(AuthResult::Success),
                Ok(
                    fail @ AuthResult::Failure {
                        partial_success: true,
                        ..
                    },
                ) => return Ok(fail),
                Ok(fail) => last_failure = fail,
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    // 2. 收集候选本机私钥路径：主机显式配置 → 密钥库额外路径 → 自动发现 ~/.ssh/*
    let mut candidates: Vec<PathBuf> = Vec::new();
    let explicit_key_configured = host
        .private_key_path
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty());
    if let Some(p) = &host.private_key_path {
        if !p.trim().is_empty() {
            candidates.push(expand_tilde(p.trim()));
        }
    }
    // 若用户在「密钥」模式下明确指定了单一私钥，优先只试该私钥，避免过多尝试触发服务端断连
    if host.auth_method == AuthMethod::Auto || !explicit_key_configured {
        for extra in extra_key_paths {
            let p = expand_tilde(extra.trim());
            if !candidates.contains(&p) {
                candidates.push(p);
            }
        }
        for local_key in discover_local_ssh_keys() {
            if !candidates.contains(&local_key) {
                candidates.push(local_key);
            }
        }
    }

    for path in candidates {
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("key")
            .to_string();

        // 防止误选 `.pub` 公钥文件作为私钥（参照 Tabby ssh.ts:229）
        if path.extension().and_then(|e| e.to_str()) == Some("pub")
            || load_public_key(&path).is_ok()
        {
            if let Some(io) = auth_io.as_deref() {
                io.notice(trf("svc.ssh.auth_pub_skipped", &[("name", &name)]));
            }
            continue;
        }

        let mut loaded =
            load_secret_key(&path, None).or_else(|_| load_secret_key(&path, passphrase));
        // 若为加密私钥且用户显式指定了该私钥或选择了「密钥」模式，直接在终端内提示输入私钥口令
        if loaded.is_err() && (explicit_key_configured || host.auth_method == AuthMethod::PrivateKey)
        {
            if let Some(io) = auth_io.as_deref_mut() {
                let prompt = trf("svc.ssh.auth_prompt_passphrase", &[("name", &name)]);
                if let Some(entered) = io.prompt(prompt, false).await {
                    loaded = load_secret_key(&path, Some(entered.as_str()));
                }
            }
        }

        if let Ok(secret_key) = loaded {
            if let Some(io) = auth_io.as_deref() {
                io.notice(trf("svc.ssh.auth_trying_key", &[("name", &name)]));
            }
            *pk_attempts += 1;
            let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(secret_key), hash_alg);
            match session
                .authenticate_publickey(&host.username, key_with_hash)
                .await
            {
                Ok(AuthResult::Success) => return Ok(AuthResult::Success),
                Ok(
                    fail @ AuthResult::Failure {
                        partial_success: true,
                        ..
                    },
                ) => return Ok(fail),
                Ok(fail) => last_failure = fail,
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    Ok(last_failure)
}

async fn try_ssh_agent(
    session: &mut client::Handle<HoloClientHandler>,
    host: &HostProfile,
    pk_attempts: &mut usize,
) -> Result<AuthResult, String> {
    let mut last_failure = AuthResult::Failure {
        remaining_methods: MethodSet::empty(),
        partial_success: false,
    };
    let Ok(mut agent) = russh::keys::agent::client::AgentClient::connect_env().await else {
        return Ok(last_failure);
    };
    let Ok(identities) = agent.request_identities().await else {
        return Ok(last_failure);
    };
    let hash_alg = session
        .best_supported_rsa_hash()
        .await
        .ok()
        .flatten()
        .flatten();

    // 若主机指定了密钥路径，参照 Tabby 优先在 Agent 中匹配对应公钥身份，避免盲目枚举触发上限
    let preferred_pub = host
        .private_key_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|raw| {
            let expanded = expand_tilde(raw);
            let pub_path = if raw.ends_with(".pub") {
                expanded
            } else {
                PathBuf::from(format!("{}.pub", expanded.display()))
            };
            load_public_key(pub_path).ok()
        });

    let mut ordered_keys = Vec::with_capacity(identities.len());
    for identity in identities {
        if let russh::keys::agent::AgentIdentity::PublicKey { key, .. } = identity {
            if preferred_pub
                .as_ref()
                .is_some_and(|pref| pref.key_data() == key.key_data())
            {
                ordered_keys.insert(0, key);
            } else {
                ordered_keys.push(key);
            }
        }
    }

    for key in ordered_keys {
        *pk_attempts += 1;
        match session
            .authenticate_publickey_with(&host.username, key, hash_alg, &mut agent)
            .await
        {
            Ok(AuthResult::Success) => return Ok(AuthResult::Success),
            Ok(
                fail @ AuthResult::Failure {
                    partial_success: true,
                    ..
                },
            ) => return Ok(fail),
            Ok(fail) => last_failure = fail,
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(last_failure)
}

async fn try_keyboard_interactive(
    session: &mut client::Handle<HoloClientHandler>,
    username: &str,
    saved_password: Option<&str>,
    mut auth_io: Option<&mut TerminalAuthIo<'_>>,
) -> Result<AuthResult, String> {
    let mut response = session
        .authenticate_keyboard_interactive_start(username, None)
        .await
        .map_err(|e| e.to_string())?;
    let mut used_saved_pwd = false;

    for _ in 0..8 {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(AuthResult::Success),
            KeyboardInteractiveAuthResponse::Failure {
                remaining_methods,
                partial_success,
            } => {
                return Ok(AuthResult::Failure {
                    remaining_methods,
                    partial_success,
                });
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                // 展示服务端下发的标题与说明（例如 Duo Push / PAM / 2FA 引导文案）
                if let Some(io) = auth_io.as_deref() {
                    if !name.trim().is_empty() {
                        io.notice(name.trim());
                    }
                    for line in instructions.lines() {
                        if !line.trim().is_empty() {
                            io.notice(line.trim());
                        }
                    }
                }

                // OpenSSH 可能下发空 prompts 列表，直接回传空数组确认即可（参照 Tabby ssh.ts:760）
                let mut answers = Vec::with_capacity(prompts.len());
                if prompts.len() == 1
                    && !prompts[0].echo
                    && is_password_prompt_text(&prompts[0].prompt)
                    && !used_saved_pwd
                    && saved_password.is_some_and(|p| !p.is_empty())
                {
                    used_saved_pwd = true;
                    answers.push(saved_password.unwrap_or_default().to_string());
                } else {
                    for p in &prompts {
                        let Some(io) = auth_io.as_deref_mut() else {
                            answers.push(saved_password.unwrap_or_default().to_string());
                            continue;
                        };
                        let label = if p.prompt.trim().is_empty() {
                            "Verification: ".to_string()
                        } else if p.prompt.ends_with(' ') {
                            p.prompt.clone()
                        } else {
                            format!("{} ", p.prompt)
                        };
                        let Some(val) = io.prompt(label, p.echo).await else {
                            return Ok(AuthResult::Failure {
                                remaining_methods: MethodSet::empty(),
                                partial_success: false,
                            });
                        };
                        answers.push(val);
                    }
                }

                response = session
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    Ok(AuthResult::Failure {
        remaining_methods: MethodSet::empty(),
        partial_success: false,
    })
}

async fn ensure_sftp(
    session: &client::Handle<HoloClientHandler>,
    slot: &AsyncMutex<Option<Arc<SftpSession>>>,
) -> Result<Arc<SftpSession>, String> {
    let mut guard = slot.lock().await;
    if let Some(existing) = guard.as_ref() {
        return Ok(Arc::clone(existing));
    }
    let ch = session
        .channel_open_session()
        .await
        .map_err(|e| trf("svc.ssh.sftp_chan_err", &[("err", &e.to_string())]))?;
    ch.request_subsystem(true, "sftp")
        .await
        .map_err(|e| trf("svc.ssh.sftp_sub_err", &[("err", &e.to_string())]))?;
    // 启用 24 并发读写管道窗口，高延迟跨洲服务器 SFTP 提速 5~20x（彻底解决 Tabby #8972）
    let sftp_cfg = SftpConfig {
        max_packet_len: 262_144,
        max_concurrent_reads: 24,
        max_concurrent_writes: 24,
        max_write_packet_len: 32_768,
        request_timeout_secs: 30,
    };
    let sftp = Arc::new(
        SftpSession::new_with_config(ch.into_stream(), sftp_cfg)
            .await
            .map_err(|e| trf("svc.ssh.sftp_init_err", &[("err", &e.to_string())]))?,
    );
    *guard = Some(Arc::clone(&sftp));
    Ok(sftp)
}

async fn list_remote_dir(
    sftp: &SftpSession,
    requested_path: &str,
) -> Result<(String, Vec<SftpEntry>), String> {
    let target = if requested_path.is_empty() || requested_path == "~" {
        sftp.canonicalize(".")
            .await
            .unwrap_or_else(|_| "/".to_string())
    } else {
        sftp.canonicalize(requested_path)
            .await
            .unwrap_or_else(|_| requested_path.to_string())
    };

    let read_dir = sftp
        .read_dir(&target)
        .await
        .map_err(|e| trf("svc.ssh.sftp_list_err", &[("path", &target), ("err", &e.to_string())]))?;

    let mut dirs_vec = Vec::new();
    let mut files_vec = Vec::new();

    for entry in read_dir {
        let raw_name = entry.file_name();
        if raw_name == "." || raw_name == ".." {
            continue;
        }
        let meta = entry.metadata();
        // 支持软链接指向目录（规避 Tabby #7638）
        let mut is_dir = entry.file_type().is_dir() || meta.is_dir();
        if !is_dir && entry.file_type().is_symlink() {
            let child_path = if target == "/" {
                format!("/{raw_name}")
            } else {
                format!("{}/{raw_name}", target.trim_end_matches('/'))
            };
            if let Ok(target_meta) = sftp.metadata(&child_path).await {
                if target_meta.is_dir() {
                    is_dir = true;
                }
            }
        }
        let display_name = if is_dir {
            format!("{raw_name}/")
        } else {
            raw_name.clone()
        };
        let item = SftpEntry {
            name: display_name,
            is_dir,
            size_display: format_file_size(meta.size.unwrap_or(0), is_dir),
            modified_display: format_mtime(meta.mtime),
            permissions: format_unix_permissions(meta.permissions, is_dir),
            is_hidden: raw_name.starts_with('.'),
        };
        if is_dir {
            dirs_vec.push(item);
        } else {
            files_vec.push(item);
        }
    }

    dirs_vec.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files_vec.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    let mut result = Vec::with_capacity(dirs_vec.len() + files_vec.len() + 1);
    if target != "/" {
        result.push(SftpEntry {
            name: "../".into(),
            is_dir: true,
            size_display: tr("sftp.parent_label").into(),
            modified_display: "—".into(),
            permissions: "drwxr-xr-x".into(),
            is_hidden: true,
        });
    }
    result.extend(dirs_vec);
    result.extend(files_vec);
    Ok((target, result))
}

#[allow(clippy::too_many_arguments)]
async fn upload_via_sftp(
    sftp: &SftpSession,
    pane_id: &str,
    transfer_id: u64,
    local_path: &PathBuf,
    remote_dir: &str,
    event_tx: &mpsc::Sender<SshEvent>,
    cancel_flag: &AtomicBool,
    task_cancel: &AtomicBool,
) -> Result<(), String> {
    let file_name = local_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| tr("svc.ssh.invalid_filename").to_string())?
        .to_string();

    let mut local_file = tokio::fs::File::open(local_path)
        .await
        .map_err(|e| trf("svc.ssh.open_local_err", &[("err", &e.to_string())]))?;
    let total_bytes = local_file
        .metadata()
        .await
        .map(|m| m.len())
        .unwrap_or(0)
        .max(1);

    let remote_path = if remote_dir.ends_with('/') {
        format!("{remote_dir}{file_name}")
    } else {
        format!("{remote_dir}/{file_name}")
    };

    // 安全原子上传：先以 CREATE|TRUNCATE|WRITE 写入随机临时文件，完整落盘后再 rename 替换（规避 Tabby #11899）
    let mut rand_bytes = [0u8; 4];
    let _ = getrandom::fill(&mut rand_bytes);
    let tmp_remote_path = format!("{remote_path}.holossh.{}.tmp", hex::encode(rand_bytes));

    let mut remote_file = sftp
        .open_with_flags(
            &tmp_remote_path,
            OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
        )
        .await
        .map_err(|e| {
            trf(
                "svc.ssh.create_remote_err",
                &[("path", &remote_path), ("err", &e.to_string())],
            )
        })?;

    // 512 KiB 缓冲配合 russh-sftp 24 并发写管道，最大化高延迟链路吞吐量
    let mut buf = vec![0u8; 512 * 1024];
    let mut sent: u64 = 0;
    let mut last_emit = std::time::Instant::now()
        .checked_sub(Duration::from_millis(60))
        .unwrap_or_else(std::time::Instant::now);

    let write_res: Result<(), String> = async {
        loop {
            if cancel_flag.load(Ordering::Relaxed) || task_cancel.load(Ordering::Relaxed) {
                return Err(tr("svc.ssh.closed").into());
            }
            let n = local_file
                .read(&mut buf)
                .await
                .map_err(|e| trf("svc.ssh.read_local_err", &[("err", &e.to_string())]))?;
            if n == 0 {
                break;
            }
            remote_file
                .write_all(&buf[..n])
                .await
                .map_err(|e| trf("svc.ssh.upload_write_err", &[("err", &e.to_string())]))?;
            sent += n as u64;
            if last_emit.elapsed() >= Duration::from_millis(32) {
                last_emit = std::time::Instant::now();
                let _ = event_tx.try_send(SshEvent::SftpTransferProgress {
                    pane_id: pane_id.to_string(),
                    transfer_id,
                    direction: TransferDirection::Upload,
                    file_name: file_name.clone(),
                    transferred_mb: sent as f32 / (1024.0 * 1024.0),
                    total_mb: total_bytes as f32 / (1024.0 * 1024.0),
                    progress: (sent as f32 / total_bytes as f32).clamp(0.0, 1.0),
                    done: false,
                });
            }
        }
        remote_file
            .flush()
            .await
            .map_err(|e| trf("svc.ssh.upload_write_err", &[("err", &e.to_string())]))?;
        let _ = remote_file.shutdown().await;
        Ok(())
    }
    .await;

    if let Err(err) = write_res {
        let _ = sftp.remove_file(&tmp_remote_path).await;
        return Err(err);
    }

    // 完整上传完毕后再 rename；若远端 SFTPv3 不支持覆盖 rename，仅在确认目标存在时移除后立即 rename
    if sftp.rename(&tmp_remote_path, &remote_path).await.is_err() {
        if sftp.metadata(&remote_path).await.is_ok() {
            let _ = sftp.remove_file(&remote_path).await;
        }
        if let Err(e) = sftp.rename(&tmp_remote_path, &remote_path).await {
            let _ = sftp.remove_file(&tmp_remote_path).await;
            return Err(trf(
                "svc.ssh.upload_write_err",
                &[("err", &e.to_string())],
            ));
        }
    }

    let _ = event_tx
        .send(SshEvent::SftpTransferProgress {
            pane_id: pane_id.to_string(),
            transfer_id,
            direction: TransferDirection::Upload,
            file_name,
            transferred_mb: total_bytes as f32 / (1024.0 * 1024.0),
            total_mb: total_bytes as f32 / (1024.0 * 1024.0),
            progress: 1.0,
            done: true,
        })
        .await;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn download_via_sftp(
    sftp: &SftpSession,
    pane_id: &str,
    transfer_id: u64,
    remote_path: &str,
    local_path: &PathBuf,
    event_tx: &mpsc::Sender<SshEvent>,
    cancel_flag: &AtomicBool,
    task_cancel: &AtomicBool,
) -> Result<(), String> {
    let file_name = remote_path
        .rsplit('/')
        .next()
        .unwrap_or("download")
        .to_string();

    let total_bytes = sftp
        .metadata(remote_path)
        .await
        .ok()
        .and_then(|m| m.size)
        .unwrap_or(1)
        .max(1);

    let mut remote_file = sftp
        .open(remote_path)
        .await
        .map_err(|e| trf("svc.ssh.open_remote_err", &[("err", &e.to_string())]))?;
    if let Some(parent) = local_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let part_seq = DOWNLOAD_PART_SEQ.fetch_add(1, Ordering::Relaxed);
    let part_name = format!(".{file_name}.{}.{part_seq}.holossh.part", std::process::id());
    let part_path = local_path
        .parent()
        .map(|p| p.join(&part_name))
        .unwrap_or_else(|| PathBuf::from(&part_name));
    let mut local_file = tokio::fs::File::create(&part_path)
        .await
        .map_err(|e| trf("svc.ssh.create_local_err", &[("err", &e.to_string())]))?;

    // 512 KiB 缓冲触发 russh-sftp 24 并发预读管道
    let mut buf = vec![0u8; 512 * 1024];
    let mut received: u64 = 0;
    let mut last_emit = std::time::Instant::now()
        .checked_sub(Duration::from_millis(60))
        .unwrap_or_else(std::time::Instant::now);

    let dl_res: Result<(), String> = async {
        loop {
            if cancel_flag.load(Ordering::Relaxed) || task_cancel.load(Ordering::Relaxed) {
                return Err(tr("svc.ssh.closed").into());
            }
            let n = remote_file
                .read(&mut buf)
                .await
                .map_err(|e| trf("svc.ssh.download_read_err", &[("err", &e.to_string())]))?;
            if n == 0 {
                break;
            }
            local_file
                .write_all(&buf[..n])
                .await
                .map_err(|e| trf("svc.ssh.write_local_err", &[("err", &e.to_string())]))?;
            received += n as u64;
            if last_emit.elapsed() >= Duration::from_millis(32) {
                last_emit = std::time::Instant::now();
                let _ = event_tx.try_send(SshEvent::SftpTransferProgress {
                    pane_id: pane_id.to_string(),
                    transfer_id,
                    direction: TransferDirection::Download,
                    file_name: file_name.clone(),
                    transferred_mb: received as f32 / (1024.0 * 1024.0),
                    total_mb: total_bytes as f32 / (1024.0 * 1024.0),
                    progress: (received as f32 / total_bytes as f32).clamp(0.0, 1.0),
                    done: false,
                });
            }
        }
        local_file
            .flush()
            .await
            .map_err(|e| trf("svc.ssh.write_local_err", &[("err", &e.to_string())]))?;
        Ok(())
    }
    .await;

    drop(local_file);
    if let Err(err) = dl_res {
        let _ = tokio::fs::remove_file(&part_path).await;
        return Err(err);
    }

    tokio::fs::rename(&part_path, local_path)
        .await
        .map_err(|e| trf("svc.ssh.write_local_err", &[("err", &e.to_string())]))?;

    let _ = event_tx
        .send(SshEvent::SftpTransferProgress {
            pane_id: pane_id.to_string(),
            transfer_id,
            direction: TransferDirection::Download,
            file_name,
            transferred_mb: total_bytes as f32 / (1024.0 * 1024.0),
            total_mb: total_bytes as f32 / (1024.0 * 1024.0),
            progress: 1.0,
            done: true,
        })
        .await;
    Ok(())
}

async fn resolve_remote_dir(sftp: &SftpSession, dir: &str) -> String {
    if dir.is_empty() || dir == "~" {
        sftp.canonicalize(".")
            .await
            .unwrap_or_else(|_| "/".to_string())
    } else if let Some(rest) = dir.strip_prefix("~/") {
        let home = sftp
            .canonicalize(".")
            .await
            .unwrap_or_else(|_| "/".to_string());
        format!("{}/{}", home.trim_end_matches('/'), rest)
    } else {
        dir.to_string()
    }
}

/// 建立单个主机自身的非跳板底层连接（直连 TCP / SOCKS5 / HTTP CONNECT）
async fn connect_direct_or_proxy(
    config: Arc<client::Config>,
    host: &HostProfile,
    timeout_secs: u64,
    auto_accept_host_key: bool,
    confirm_bridge: HostKeyConfirmBridge,
) -> Result<client::Handle<HoloClientHandler>, String> {
    let target_addr = format!("{}:{}", host.hostname, host.port);
    let handler = HoloClientHandler::new(
        &host.hostname,
        host.port,
        auto_accept_host_key,
        Some(confirm_bridge),
    );
    let dial_timeout = Duration::from_secs(timeout_secs);

    match host.connection {
        ConnectionKind::Socks5 => {
            let (proxy_host, proxy_port) = host
                .effective_proxy_host_port()
                .ok_or_else(|| tr("svc.ssh.socks5_addr_req").to_string())?;
            let proxy_addr = format!("{proxy_host}:{proxy_port}");
            let stream = tokio::time::timeout(
                dial_timeout,
                connect_via_socks5(&proxy_addr, &host.hostname, host.port),
            )
            .await
            .map_err(|_| {
                trf(
                    "svc.ssh.timeout",
                    &[("secs", &timeout_secs.to_string()), ("addr", &target_addr)],
                )
            })??;
            client::connect_stream(config, stream, handler)
                .await
                .map_err(|e| format_connect_error(&target_addr, &e, auto_accept_host_key))
        }
        ConnectionKind::HttpProxy => {
            let (proxy_host, proxy_port) = host
                .effective_proxy_host_port()
                .ok_or_else(|| tr("svc.ssh.http_addr_req").to_string())?;
            let proxy_addr = format!("{proxy_host}:{proxy_port}");
            let stream = tokio::time::timeout(
                dial_timeout,
                connect_via_http_proxy(&proxy_addr, &host.hostname, host.port),
            )
            .await
            .map_err(|_| {
                trf(
                    "svc.ssh.timeout",
                    &[("secs", &timeout_secs.to_string()), ("addr", &target_addr)],
                )
            })??;
            client::connect_stream(config, stream, handler)
                .await
                .map_err(|e| format_connect_error(&target_addr, &e, auto_accept_host_key))
        }
        ConnectionKind::Direct | ConnectionKind::Local | ConnectionKind::JumpHost => {
            let stream = tokio::time::timeout(dial_timeout, TcpStream::connect(target_addr.as_str()))
                .await
                .map_err(|_| {
                    trf(
                        "svc.ssh.timeout",
                        &[("secs", &timeout_secs.to_string()), ("addr", &target_addr)],
                    )
                })?
                .map_err(|e| {
                    trf(
                        "svc.ssh.conn_err",
                        &[("addr", &target_addr), ("err", &e.to_string())],
                    )
                })?;
            let _ = stream.set_nodelay(true);
            client::connect_stream(config, stream, handler)
                .await
                .map_err(|e| format_connect_error(&target_addr, &e, auto_accept_host_key))
        }
    }
}

/// 按主机配置建立真实底层传输：直连 TCP / SOCKS5 / HTTP CONNECT / 多级已保存跳板主机链
async fn establish_ssh_transport(
    config: Arc<client::Config>,
    host: &HostProfile,
    jump_hops: &[ResolvedJumpHop],
    extra_key_paths: &[String],
    timeout_secs: u64,
    auto_accept_host_key: bool,
    confirm_bridge: HostKeyConfirmBridge,
) -> Result<(client::Handle<HoloClientHandler>, Vec<client::Handle<HoloClientHandler>>), String> {
    let target_addr = format!("{}:{}", host.hostname, host.port);
    let handler = HoloClientHandler::new(
        &host.hostname,
        host.port,
        auto_accept_host_key,
        Some(confirm_bridge.clone()),
    );

    match host.connection {
        ConnectionKind::Direct
        | ConnectionKind::Local
        | ConnectionKind::Socks5
        | ConnectionKind::HttpProxy => {
            let session = connect_direct_or_proxy(
                config,
                host,
                timeout_secs,
                auto_accept_host_key,
                confirm_bridge,
            )
            .await?;
            Ok((session, Vec::new()))
        }
        ConnectionKind::JumpHost => {
            if jump_hops.is_empty() {
                return Err(tr("svc.ssh.jump_addr_req").to_string());
            }
            let mut guards: Vec<client::Handle<HoloClientHandler>> =
                Vec::with_capacity(jump_hops.len());
            for (idx, hop) in jump_hops.iter().enumerate() {
                let hop_addr = format!("{}:{}", hop.host.hostname, hop.host.port);
                let mut hop_session = if idx == 0 {
                    connect_direct_or_proxy(
                        config.clone(),
                        &hop.host,
                        timeout_secs,
                        auto_accept_host_key,
                        confirm_bridge.clone(),
                    )
                    .await
                    .map_err(|err| {
                        trf("svc.ssh.jump_conn_err", &[("addr", &hop_addr), ("err", &err)])
                    })?
                } else {
                    let prev = guards.last().expect("previous jump hop present");
                    let tunnel = prev
                        .channel_open_direct_tcpip(
                            &hop.host.hostname,
                            hop.host.port as u32,
                            "127.0.0.1",
                            0,
                        )
                        .await
                        .map_err(|e| {
                            trf(
                                "svc.ssh.jump_fwd_err",
                                &[("addr", &hop_addr), ("err", &e.to_string())],
                            )
                        })?;
                    let hop_handler = HoloClientHandler::new(
                        &hop.host.hostname,
                        hop.host.port,
                        auto_accept_host_key,
                        Some(confirm_bridge.clone()),
                    );
                    client::connect_stream(config.clone(), tunnel.into_stream(), hop_handler)
                        .await
                        .map_err(|e| {
                            trf(
                                "svc.ssh.jump_conn_err",
                                &[("addr", &hop_addr), ("err", &e.to_string())],
                            )
                        })?
                };

                let hop_pwd = hop.password.as_deref().filter(|s| !s.is_empty());
                let hop_pem = hop.key_pem.as_deref().filter(|s| !s.trim().is_empty());
                let authed = authenticate_session(
                    &mut hop_session,
                    &hop.host,
                    hop_pwd,
                    hop_pem,
                    extra_key_paths,
                    None,
                )
                .await
                .unwrap_or(false);
                if !authed {
                    return Err(trf("svc.ssh.jump_auth_err", &[("addr", &hop_addr)]));
                }
                guards.push(hop_session);
            }

            let last_hop = guards.last().expect("jump_hops non-empty");
            let tunnel = last_hop
                .channel_open_direct_tcpip(&host.hostname, host.port as u32, "127.0.0.1", 0)
                .await
                .map_err(|e| {
                    trf(
                        "svc.ssh.jump_fwd_err",
                        &[("addr", &target_addr), ("err", &e.to_string())],
                    )
                })?;
            let session = client::connect_stream(config, tunnel.into_stream(), handler)
                .await
                .map_err(|e| format_connect_error(&target_addr, &e, auto_accept_host_key))?;
            Ok((session, guards))
        }
    }
}

fn format_connect_error(addr: &str, err: &russh::Error, auto_accept: bool) -> String {
    let raw = err.to_string();
    if raw.contains("UnknownKey") || raw.contains("unknown") {
        if !auto_accept {
            return trf("svc.ssh.hostkey_mismatch", &[("addr", addr)]);
        }
    }
    trf("svc.ssh.conn_err", &[("addr", addr), ("err", &raw)])
}

/// 标准 RFC 1928 SOCKS5 隧道握手
async fn connect_via_socks5(
    proxy_addr: &str,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream, String> {
    let mut stream = TcpStream::connect(proxy_addr)
        .await
        .map_err(|e| trf("svc.ssh.socks5_conn_err", &[("addr", proxy_addr), ("err", &e.to_string())]))?;

    // 协商无认证方式 (VER=5, NMETHODS=1, METHOD=0x00)
    stream
        .write_all(&[0x05, 0x01, 0x00])
        .await
        .map_err(|e| trf("svc.ssh.socks5_hs_send", &[("err", &e.to_string())]))?;
    let mut greeting_resp = [0u8; 2];
    stream
        .read_exact(&mut greeting_resp)
        .await
        .map_err(|e| trf("svc.ssh.socks5_hs_read", &[("err", &e.to_string())]))?;
    if greeting_resp[0] != 0x05 || greeting_resp[1] != 0x00 {
        return Err(trf(
            "svc.ssh.socks5_no_auth_rej",
            &[("code", &format!("{:02x}", greeting_resp[1]))],
        ));
    }

    // 发送 CONNECT 请求（DOMAINNAME 模式，兼容域名与 IP）
    let host_bytes = target_host.as_bytes();
    if host_bytes.len() > 255 {
        return Err(tr("svc.ssh.socks5_host_len").into());
    }
    let mut req = Vec::with_capacity(7 + host_bytes.len());
    req.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, host_bytes.len() as u8]);
    req.extend_from_slice(host_bytes);
    req.extend_from_slice(&target_port.to_be_bytes());
    stream
        .write_all(&req)
        .await
        .map_err(|e| trf("svc.ssh.socks5_req_err", &[("err", &e.to_string())]))?;

    let mut head = [0u8; 4];
    stream
        .read_exact(&mut head)
        .await
        .map_err(|e| trf("svc.ssh.socks5_resp_err", &[("err", &e.to_string())]))?;
    if head[0] != 0x05 || head[1] != 0x00 {
        return Err(trf(
            "svc.ssh.socks5_target_err",
            &[("code", &format!("{:02x}", head[1]))],
        ));
    }
    let addr_len = match head[3] {
        0x01 => 4usize,
        0x04 => 16usize,
        0x03 => {
            let mut l = [0u8; 1];
            stream
                .read_exact(&mut l)
                .await
                .map_err(|e| trf("svc.ssh.socks5_addr_read", &[("err", &e.to_string())]))?;
            l[0] as usize
        }
        _ => return Err(tr("svc.ssh.socks5_addr_unknown").into()),
    };
    let mut discard = vec![0u8; addr_len + 2];
    stream
        .read_exact(&mut discard)
        .await
        .map_err(|e| trf("svc.ssh.socks5_bind_read", &[("err", &e.to_string())]))?;
    Ok(stream)
}

/// 标准 HTTP CONNECT 隧道握手
async fn connect_via_http_proxy(
    proxy_addr: &str,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream, String> {
    let clean_proxy = proxy_addr
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    let mut stream = TcpStream::connect(clean_proxy)
        .await
        .map_err(|e| trf("svc.ssh.http_conn_err", &[("addr", clean_proxy), ("err", &e.to_string())]))?;

    let req = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\nProxy-Connection: Keep-Alive\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| trf("svc.ssh.http_send_err", &[("err", &e.to_string())]))?;

    let mut resp = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while resp.len() < 4096 {
        stream
            .read_exact(&mut byte)
            .await
            .map_err(|e| trf("svc.ssh.http_read_err", &[("err", &e.to_string())]))?;
        resp.push(byte[0]);
        if resp.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let status_line = String::from_utf8_lossy(&resp)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    if !status_line.contains(" 200") {
        return Err(trf("svc.ssh.http_rejected", &[("status", &status_line)]));
    }
    Ok(stream)
}

fn parent_remote_dir(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    let idx = trimmed.rfind('/')?;
    if idx == 0 {
        Some("/".to_string())
    } else {
        Some(trimmed[..idx].to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_parent_remote_dir() {
        assert_eq!(parent_remote_dir("/var/log/nginx"), Some("/var/log".into()));
        assert_eq!(parent_remote_dir("/etc"), Some("/".into()));
    }

    #[test]
    fn chains_partial_success_2fa_auth_plan() {
        let mut queue = build_initial_auth_queue(AuthMethod::PrivateKey, false);
        let mut allowed = vec![MethodKind::PublicKey];
        assert_eq!(select_next_auth_step(&queue, &allowed), Some(0));
        queue.remove(0);

        // 模拟私钥通过但服务端要求二次键盘交互认证（2FA/OTP）
        let rem = MethodSet::from(&[MethodKind::KeyboardInteractive][..]);
        update_auth_plan_after_failure(&mut queue, &mut allowed, &rem, true, false);
        assert_eq!(allowed, vec![MethodKind::KeyboardInteractive]);
        assert_eq!(queue, vec![AuthStep::KeyboardInteractive]);
        assert_eq!(select_next_auth_step(&queue, &allowed), Some(0));
    }
}
