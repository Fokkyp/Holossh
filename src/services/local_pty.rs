//! 原生本地 PTY 终端会话服务（直接拉起本机 `$SHELL`，无需开启 `sshd`）
//!
//! 复用 `PaneSessionHandle` / `SshCommand` / `SshEvent` 统一会话协议，
//! 让本地终端与远端 SSH 窗格 100% 共享分屏、搜索、快捷键、缩放与流控背压机制。

use crate::i18n::tr;
use crate::services::ssh::{PaneSessionHandle, SshCommand, SshEvent, runtime};
use std::path::PathBuf;
use tokio::sync::mpsc;

/// 获取当前系统默认本地 Shell 名称（用于侧边栏副标题展示，如 `zsh` / `bash`）
pub fn default_local_shell_name() -> String {
    let path = resolve_local_shell_path();
    path.file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("shell")
        .to_string()
}

/// 解析本机登录 Shell 路径（优先 `$SHELL`，依次回退 `/bin/zsh`、`/bin/bash`、`/bin/sh`）
pub fn resolve_local_shell_path() -> PathBuf {
    if let Ok(shell) = std::env::var("SHELL") {
        let trimmed = shell.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            if p.exists() {
                return p;
            }
        }
    }
    for candidate in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return p;
        }
    }
    PathBuf::from("/bin/sh")
}

/// 启动一个原生本地 PTY 会话，返回与 SSH 会话一致的 `PaneSessionHandle`
pub fn spawn_local_pty_session(
    pane_id: String,
    host_id: String,
    cols: u32,
    rows: u32,
    event_tx: mpsc::Sender<SshEvent>,
) -> PaneSessionHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SshCommand>();

    #[cfg(unix)]
    {
        runtime().spawn(async move {
            run_unix_local_pty(pane_id, host_id, cols, rows, cmd_rx, event_tx).await;
        });
    }

    #[cfg(not(unix))]
    {
        let _ = (cols, rows, cmd_rx);
        runtime().spawn(async move {
            let _ = event_tx
                .send(SshEvent::Error {
                    pane_id,
                    host_id,
                    message: "Local PTY is currently only supported on Unix/macOS.".to_string(),
                })
                .await;
        });
    }

    PaneSessionHandle { tx: cmd_tx }
}

#[cfg(unix)]
async fn run_unix_local_pty(
    pane_id: String,
    host_id: String,
    cols: u32,
    rows: u32,
    mut cmd_rx: mpsc::UnboundedReceiver<SshCommand>,
    event_tx: mpsc::Sender<SshEvent>,
) {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let init_cols = cols.clamp(2, u16::MAX as u32) as u16;
    let init_rows = rows.clamp(2, u16::MAX as u32) as u16;

    // 1. 分配原生 POSIX PTY 主从设备对
    let mut master_raw: libc::c_int = -1;
    let mut slave_raw: libc::c_int = -1;
    let ws = libc::winsize {
        ws_row: init_rows,
        ws_col: init_cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let open_rc = unsafe {
        libc::openpty(
            &mut master_raw,
            &mut slave_raw,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &ws as *const libc::winsize as *mut libc::winsize,
        )
    };
    if open_rc != 0 || master_raw < 0 || slave_raw < 0 {
        let err = std::io::Error::last_os_error();
        let _ = event_tx
            .send(SshEvent::Error {
                pane_id,
                host_id,
                message: format!("无法创建本地 PTY：{err}"),
            })
            .await;
        return;
    }

    // 主端标记 FD_CLOEXEC，防止子进程继承 master fd 导致退出后无法 EOF
    unsafe {
        set_cloexec(master_raw);
        set_cloexec(slave_raw);
    }

    let master_fd = unsafe { OwnedFd::from_raw_fd(master_raw) };
    let slave_fd = unsafe { OwnedFd::from_raw_fd(slave_raw) };

    // 为子进程准备 stdin/stdout/stderr（复制自 slave_fd）
    let Ok(stdin_fd) = slave_fd.try_clone() else {
        let _ = event_tx
            .send(SshEvent::Error {
                pane_id,
                host_id,
                message: "无法复制本地 PTY 标准输入描述符".to_string(),
            })
            .await;
        return;
    };
    let Ok(stdout_fd) = slave_fd.try_clone() else {
        let _ = event_tx
            .send(SshEvent::Error {
                pane_id,
                host_id,
                message: "无法复制本地 PTY 标准输出描述符".to_string(),
            })
            .await;
        return;
    };
    let Ok(stderr_fd) = slave_fd.try_clone() else {
        let _ = event_tx
            .send(SshEvent::Error {
                pane_id,
                host_id,
                message: "无法复制本地 PTY 标准错误描述符".to_string(),
            })
            .await;
        return;
    };

    let shell_path = resolve_local_shell_path();
    let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));

    let mut cmd = Command::new(&shell_path);
    cmd.arg("-l")
        .current_dir(&home_dir)
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .env("TERM_PROGRAM", "Holossh")
        .stdin(Stdio::from(stdin_fd))
        .stdout(Stdio::from(stdout_fd))
        .stderr(Stdio::from(stderr_fd));

    // 确保 macOS GUI 启动场景下 UTF-8 / CJK 环境变量完整，避免中文乱码
    let lang_missing = std::env::var("LANG")
        .map(|v| v.trim().is_empty())
        .unwrap_or(true);
    if lang_missing {
        cmd.env("LANG", "en_US.UTF-8");
    }

    unsafe {
        cmd.pre_exec(|| {
            // 新建会话并将 slave PTY（fd 0）设为控制终端，支持 Ctrl+C / 作业控制 / vim / sudo
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            for sig in [
                libc::SIGPIPE,
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTSTP,
                libc::SIGTTIN,
                libc::SIGTTOU,
                libc::SIGCHLD,
            ] {
                libc::signal(sig, libc::SIG_DFL);
            }
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = event_tx
                .send(SshEvent::Error {
                    pane_id,
                    host_id,
                    message: format!("启动本地 Shell（{}）失败：{err}", shell_path.display()),
                })
                .await;
            return;
        }
    };

    // 父进程必须立即关闭 slave_fd，子进程退出时 master_fd 才能立即收到 EOF/EIO
    drop(slave_fd);

    let child_pid = child.id() as libc::pid_t;

    // 复制 master_fd 分离读/写通道，读写互不阻塞且支持安全关闭
    let Ok(master_read_fd) = master_fd.try_clone() else {
        let _ = child.kill();
        let _ = child.wait();
        let _ = event_tx
            .send(SshEvent::Error {
                pane_id,
                host_id,
                message: "无法初始化本地 PTY 读取句柄".to_string(),
            })
            .await;
        return;
    };
    let Ok(master_write_fd) = master_fd.try_clone() else {
        let _ = child.kill();
        let _ = child.wait();
        let _ = event_tx
            .send(SshEvent::Error {
                pane_id,
                host_id,
                message: "无法初始化本地 PTY 写入句柄".to_string(),
            })
            .await;
        return;
    };

    // 通知 UI 会话已就绪（自动清屏并点亮在线圆点）
    let _ = event_tx
        .send(SshEvent::Connected {
            pane_id: pane_id.clone(),
            host_id: host_id.clone(),
        })
        .await;

    let closing = Arc::new(AtomicBool::new(false));
    let (eof_tx, mut eof_rx) = tokio::sync::oneshot::channel::<()>();

    // 2. 专用读取线程：将本地 PTY 字节流通过有界背压通道回传 UI
    let read_pane_id = pane_id.clone();
    let read_event_tx = event_tx.clone();
    let read_closing = Arc::clone(&closing);
    let _ = std::thread::Builder::new()
        .name(format!("local-pty-rx-{pane_id}"))
        .spawn(move || {
            let mut reader = File::from(master_read_fd);
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if read_closing.load(Ordering::Relaxed) {
                            break;
                        }
                        if read_event_tx
                            .blocking_send(SshEvent::Output {
                                pane_id: read_pane_id.clone(),
                                data: buf[..n].to_vec(),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            let _ = eof_tx.send(());
        });

    // 3. 专用写入线程：避免键盘输入在子进程暂停读取（如 Ctrl+S）时阻塞 Tokio worker
    let (write_tx, write_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let _ = std::thread::Builder::new()
        .name(format!("local-pty-tx-{pane_id}"))
        .spawn(move || {
            let mut writer = File::from(master_write_fd);
            while let Ok(bytes) = write_rx.recv() {
                if bytes.is_empty() {
                    continue;
                }
                if writer.write_all(&bytes).is_err() {
                    break;
                }
            }
        });

    // 4. 控制循环：转发输入、同步 PTY 窗口尺寸、处理主动断开或子进程自然退出
    let mut disconnected_by_user = false;
    loop {
        tokio::select! {
            biased;
            cmd_opt = cmd_rx.recv() => {
                match cmd_opt {
                    Some(SshCommand::Write(bytes)) => {
                        if write_tx.send(bytes).is_err() {
                            break;
                        }
                    }
                    Some(SshCommand::Resize { cols, rows }) => {
                        let next_ws = libc::winsize {
                            ws_row: rows.clamp(2, u16::MAX as u32) as u16,
                            ws_col: cols.clamp(2, u16::MAX as u32) as u16,
                            ws_xpixel: 0,
                            ws_ypixel: 0,
                        };
                        unsafe {
                            let _ = libc::ioctl(
                                master_fd.as_raw_fd(),
                                libc::TIOCSWINSZ as _,
                                &next_ws,
                            );
                        }
                    }
                    Some(SshCommand::Disconnect) | None => {
                        disconnected_by_user = true;
                        break;
                    }
                    Some(
                        SshCommand::SftpList { .. }
                        | SshCommand::SftpCreateDir { .. }
                        | SshCommand::SftpDelete { .. }
                        | SshCommand::SftpUpload { .. }
                        | SshCommand::SftpDownload { .. }
                        | SshCommand::SftpCancelTransfer { .. },
                    ) => {
                        let _ = event_tx
                            .send(SshEvent::SftpError {
                                pane_id: pane_id.clone(),
                                message: tr("toast.sftp_disabled_local").to_string(),
                            })
                            .await;
                    }
                    Some(SshCommand::AuthResponse { .. } | SshCommand::HostKeyResponse { .. }) => {}
                }
            }
            _ = &mut eof_rx => {
                break;
            }
        }
    }

    closing.store(true, Ordering::Relaxed);
    drop(write_tx);

    if disconnected_by_user {
        // 先向会话进程组发送 SIGHUP 优雅通知 Shell 退出，随后关闭主端并回收子进程
        unsafe {
            let _ = libc::kill(-child_pid, libc::SIGHUP);
        }
        drop(master_fd);
        let _ = tokio::task::spawn_blocking(move || {
            for _ in 0..10 {
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
                    Err(_) => return,
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        })
        .await;
    } else {
        drop(master_fd);
        let exit_status = tokio::task::spawn_blocking(move || child.wait())
            .await
            .ok()
            .and_then(|r| r.ok());
        let reason = match exit_status.and_then(|s| s.code()) {
            Some(0) | None => tr("workspace.local_shell_exited").to_string(),
            Some(code) => format!("{} (exit {code})", tr("workspace.local_shell_exited")),
        };
        let _ = event_tx
            .send(SshEvent::Disconnected {
                pane_id,
                host_id,
                reason,
                unexpected: false,
            })
            .await;
    }
}

#[cfg(unix)]
unsafe fn set_cloexec(fd: libc::c_int) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            let _ = libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn resolves_existing_local_shell() {
        let shell = resolve_local_shell_path();
        assert!(shell.exists(), "expected local shell to exist: {shell:?}");
        assert!(!default_local_shell_name().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn local_pty_runs_commands_resizes_blocks_sftp_and_exits_cleanly() {
        let (tx, mut rx) = mpsc::channel::<SshEvent>(64);
        let handle = spawn_local_pty_session(
            "pane-local-test".to_string(),
            "host-local-test".to_string(),
            80,
            24,
            tx,
        );

        let result = runtime().block_on(async {
            let mut connected = false;
            let mut got_sftp_error = false;
            let mut got_disconnected = false;
            let mut unexpected_flag = true;
            let mut output = String::new();
            let mut sent_cmd = false;

            let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
            while tokio::time::Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                let Ok(Some(ev)) = tokio::time::timeout(remaining, rx.recv()).await else {
                    break;
                };
                match ev {
                    SshEvent::Connected { pane_id, host_id } => {
                        assert_eq!(pane_id, "pane-local-test");
                        assert_eq!(host_id, "host-local-test");
                        connected = true;
                        handle.resize(100, 30);
                        handle.sftp_list("~".to_string());
                        if !sent_cmd {
                            sent_cmd = true;
                            handle.send_bytes(b"echo HOLOSH_LOCAL_PTY_OK; exit\n".to_vec());
                        }
                    }
                    SshEvent::Output { data, .. } => {
                        output.push_str(&String::from_utf8_lossy(&data));
                    }
                    SshEvent::SftpError { .. } => {
                        got_sftp_error = true;
                    }
                    SshEvent::Disconnected { unexpected, .. } => {
                        got_disconnected = true;
                        unexpected_flag = unexpected;
                        break;
                    }
                    SshEvent::Error { message, .. } => {
                        panic!("unexpected local PTY error: {message}");
                    }
                    _ => {}
                }
            }
            (
                connected,
                got_sftp_error,
                got_disconnected,
                unexpected_flag,
                output,
            )
        });

        let (connected, got_sftp_error, got_disconnected, unexpected_flag, output) = result;
        assert!(connected, "expected Connected event");
        assert!(got_sftp_error, "expected SftpError when requesting SFTP on local PTY");
        assert!(
            output.contains("HOLOSH_LOCAL_PTY_OK"),
            "expected command output in local PTY stream, got: {output:?}"
        );
        assert!(got_disconnected, "expected clean Disconnected event after exit");
        assert!(!unexpected_flag, "normal shell exit must have unexpected=false");
    }
}
