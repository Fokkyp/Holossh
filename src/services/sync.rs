//! HoloSSH 全量端到端加密云端同步引擎（v2）
//!
//! 设计原则：
//! - **100% 端到端加密**：主机列表、IP/端口/用户名、分组、删除墓碑与密钥库凭据全部加密后上云，
//!   云端（GitHub Gist / S3）不出现任何明文资产信息；
//! - **同步凭证与密钥库解耦**：GitHub Token 与 S3 Secret 直接来自应用配置（`settings.json`），
//!   锁定态也可正常拉取/推送，绝不上传至云端；
//! - **Pull-Before-Push + CRDT 无损合并**：
//!   - 主机按 UUID v4 + `updated_at_ms` 合并，配合 `deleted_hosts` / `deleted_groups` 墓碑彻底杜绝删后“诈尸”；
//!   - 推送前先拉取远端版本：已解锁时自动合并并压缩快照；锁定态若远端已更新且同库，
//!     保留远端密文并将本地变更封存为 `sealed_patches` 追加上传，绝不盲目覆盖其他设备数据；
//!   - 锁定态拉取只暂存到 `pending_sync_pull.json`，绝不覆盖本地未推送的 `vault.json`；
//! - **传输层完整兼容**：GitHub Gist 支持超 1MB `truncated` → `raw_url` 拉取；
//!   S3 支持 AWS SigV4 路径规范编码与自动识别 Virtual-Hosted / Path-Style。

use crate::i18n::{tr, trf};
use crate::services::ssh::runtime;
use crate::services::vault::{
    self, EncryptedVaultFile, SealedBlob, VaultData,
};
use crate::state::host::{HostProfile, generate_host_uuid, now_unix_ms};
use crate::state::settings::{AppSettings, SyncProvider};
use chrono::{Local, TimeZone, Utc};
use hmac::{Hmac, Mac};
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, USER_AGENT,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use url::Url;

type HmacSha256 = Hmac<Sha256>;

pub const SYNC_SNAPSHOT_FORMAT: &str = "holossh-e2ee-sync-v2";

/// 同步动作类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncAction {
    TestConnection,
    /// 保存配置后探测云端是否已有快照
    ProbeRemote,
    Push,
    /// 强制以本机覆盖云端（生成全新 sync_epoch，通知其他设备弹窗决策）
    ForcePush,
    Pull,
    AutoPull,
}

/// 触发同步决策弹窗的冲突类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncConflictReason {
    /// 首次配置/尚未绑定同步纪元时，检测到云端已有配置快照
    InitialBind,
    /// 本机已绑定过云端，但云端被其他设备强制覆盖或更换了密钥库
    RemoteDiverged,
}

/// 参与端到端加密同步的完整主机与分组目录（仅存在于内存与密文内，绝不明文上云）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncCatalog {
    #[serde(default)]
    pub hosts: Vec<HostProfile>,
    #[serde(default)]
    pub deleted_hosts: BTreeMap<String, u64>,
    #[serde(default)]
    pub custom_groups: Vec<String>,
    #[serde(default)]
    pub group_updated_at: BTreeMap<String, u64>,
    #[serde(default)]
    pub deleted_groups: BTreeMap<String, u64>,
}

/// 云端存储的 100% 端到端加密同步快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedSyncSnapshot {
    pub format: String,
    pub revision: u64,
    /// 同步纪元 UUID：正常增量推送保持不变；强制覆盖云端时生成新 UUID，迫使其他设备暂停自动同步并弹窗确认
    #[serde(default)]
    pub sync_epoch: String,
    pub updated_at_ms: u64,
    /// Argon2id + AES-256-GCM 加密密钥库信封（附带 X25519 写入公钥与锁定态封存凭据）
    pub vault: EncryptedVaultFile,
    /// 使用 `vault.write_public_key_hex` 封装的全量 `SyncCatalog` 密文
    pub sealed_catalog: SealedBlob,
    /// 锁定态并发推送时追加的增量 `SyncCatalog` 密文补丁（任意设备解锁后自动合并压缩）
    #[serde(default)]
    pub sealed_patches: Vec<SealedBlob>,
}

/// 发起异步云同步任务所需的输入上下文
pub struct SyncRequest {
    pub action: SyncAction,
    pub settings: AppSettings,
    pub local_catalog: SyncCatalog,
    pub local_vault_file: Option<EncryptedVaultFile>,
    pub unlocked_key: Option<[u8; 32]>,
    pub unlocked_vault_data: Option<VaultData>,
}

/// 异步云端同步结果事件
#[derive(Debug, Clone)]
pub enum SyncEvent {
    TestOk {
        message: String,
    },
    /// 保存配置后探测发现云端为空，可直接上传本地作为初始版本
    ProbeEmpty,
    /// 检测到首次绑定云端已有数据，或跨设备发生强制覆盖/换库分叉，需弹窗由用户决策
    ConflictPrompt {
        reason: SyncConflictReason,
        snapshot: EncryptedSyncSnapshot,
        same_vault_salt: bool,
    },
    PushOk {
        message: String,
        created_gist_id: Option<String>,
        revision: u64,
        sync_epoch: String,
        vault_salt: String,
        synced_at: String,
        merged_catalog: Option<SyncCatalog>,
        merged_vault_data: Option<VaultData>,
        merged_vault_file: Option<EncryptedVaultFile>,
    },
    PullMerged {
        revision: u64,
        sync_epoch: String,
        vault_salt: String,
        synced_at: String,
        merged_catalog: SyncCatalog,
        merged_vault_data: VaultData,
        merged_vault_file: EncryptedVaultFile,
    },
    PullNeedsUnlock {
        message: String,
        snapshot: EncryptedSyncSnapshot,
        synced_at: String,
        is_auto_pull: bool,
    },
    PullUpToDate,
    Error {
        message: String,
    },
}

// ====================== 锁定态拉取暂存文件（保护本地未推送凭据） ======================

fn pending_pull_path() -> Option<PathBuf> {
    AppSettings::config_dir().map(|d| d.join("pending_sync_pull.json"))
}

pub fn save_pending_pull(snapshot: &EncryptedSyncSnapshot) {
    let Some(path) = pending_pull_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(snapshot) {
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, json).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
            }
            let _ = fs::rename(&tmp, &path);
        }
    }
}

pub fn load_pending_pull() -> Option<EncryptedSyncSnapshot> {
    let path = pending_pull_path()?;
    let raw = fs::read_to_string(path).ok()?;
    let snap: EncryptedSyncSnapshot = serde_json::from_str(&raw).ok()?;
    (snap.format == SYNC_SNAPSHOT_FORMAT).then_some(snap)
}

pub fn clear_pending_pull() {
    if let Some(path) = pending_pull_path() {
        let _ = fs::remove_file(path);
    }
}

/// 将 UTC 毫秒时间戳格式化为本机可读日期时间
pub fn format_epoch_ms_local(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let nanos = ((ms % 1000) * 1_000_000) as u32;
    Local
        .timestamp_opt(secs, nanos)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(now_local_display)
}

/// 在拉取覆盖本地前自动留存一份本地快照备份（滚动保留最近 5 份，防止误操作不可逆丢失）
pub fn create_local_sync_backup() -> Result<Option<PathBuf>, String> {
    let Some(cfg_dir) = AppSettings::config_dir() else {
        return Ok(None);
    };
    let hosts_src = cfg_dir.join("hosts.json");
    let vault_src = cfg_dir.join("vault.json");
    if !hosts_src.exists() && !vault_src.exists() {
        return Ok(None);
    }
    let backups_root = cfg_dir.join("backups");
    fs::create_dir_all(&backups_root)
        .map_err(|e| format!("create backups dir failed: {e}"))?;
    let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let mut dst_dir = backups_root.join(format!("backup-{stamp}"));
    if dst_dir.exists() {
        dst_dir = backups_root.join(format!("backup-{stamp}-{}", now_unix_ms() % 1000));
    }
    fs::create_dir_all(&dst_dir)
        .map_err(|e| format!("create backup dir failed: {e}"))?;

    if hosts_src.exists() {
        let _ = fs::copy(&hosts_src, dst_dir.join("hosts.json"));
    }
    if vault_src.exists() {
        let dst_vault = dst_dir.join("vault.json");
        if fs::copy(&vault_src, &dst_vault).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&dst_vault, fs::Permissions::from_mode(0o600));
            }
        }
    }

    // 滚动清理超出 5 份的历史备份目录
    if let Ok(entries) = fs::read_dir(&backups_root) {
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("backup-"))
            })
            .collect();
        dirs.sort();
        while dirs.len() > 5 {
            let oldest = dirs.remove(0);
            let _ = fs::remove_dir_all(oldest);
        }
    }

    Ok(Some(dst_dir))
}

/// 判定云端快照与本机是否发生「首次未绑定」或「跨设备强制覆盖/换库分叉」
pub fn detect_sync_conflict(
    settings: &AppSettings,
    local_vault: Option<&EncryptedVaultFile>,
    remote: &EncryptedSyncSnapshot,
) -> Option<SyncConflictReason> {
    if settings.sync_epoch.is_empty() {
        return Some(SyncConflictReason::InitialBind);
    }
    if !remote.sync_epoch.is_empty() && remote.sync_epoch != settings.sync_epoch {
        return Some(SyncConflictReason::RemoteDiverged);
    }
    if !settings.sync_vault_salt.is_empty() && remote.vault.salt_hex != settings.sync_vault_salt {
        return Some(SyncConflictReason::RemoteDiverged);
    }
    if local_vault.is_some_and(|lv| lv.salt_hex != remote.vault.salt_hex)
        && remote.revision > settings.sync_base_revision
    {
        return Some(SyncConflictReason::RemoteDiverged);
    }
    None
}

/// 以云端为准（拉取覆盖本地）：先自动备份本地文件，再用主密码解开远端快照并完整落盘
pub fn adopt_remote_snapshot_overwrite(
    master_password: &str,
    snapshot: &EncryptedSyncSnapshot,
) -> Result<([u8; 32], VaultData, EncryptedVaultFile, SyncCatalog), String> {
    let (remote_key, remote_vault) =
        vault::unlock_encrypted_file(&snapshot.vault, master_password)?;
    let remote_catalog = unseal_snapshot_catalog(&remote_vault, snapshot)?;
    let _ = create_local_sync_backup();
    let consolidated_vault_file =
        vault::encrypt_vault_with_template(&remote_key, &snapshot.vault, &remote_vault)?;
    vault::save_encrypted_file(&consolidated_vault_file)?;
    Ok((
        remote_key,
        remote_vault,
        consolidated_vault_file,
        remote_catalog,
    ))
}

/// 以云端为准（已解锁且同密钥直通路径）：自动备份本地后直接采用远端目录与密钥库
pub fn adopt_remote_snapshot_overwrite_with_unlocked_key(
    unlocked_key: &[u8; 32],
    snapshot: &EncryptedSyncSnapshot,
) -> Option<(VaultData, EncryptedVaultFile, SyncCatalog)> {
    let remote_vault = vault::decrypt_vault_with_key(unlocked_key, &snapshot.vault).ok()?;
    let remote_catalog = unseal_snapshot_catalog(&remote_vault, snapshot).ok()?;
    let _ = create_local_sync_backup();
    let consolidated_vault_file =
        vault::encrypt_vault_with_template(unlocked_key, &snapshot.vault, &remote_vault).ok()?;
    vault::save_encrypted_file(&consolidated_vault_file).ok()?;
    Some((remote_vault, consolidated_vault_file, remote_catalog))
}

// ====================== CRDT 目录合并 & 解封算法 ======================

/// 按 UUID + `updated_at_ms` + 删除墓碑执行确定性 LWW-Element-Set 合并
pub fn merge_catalogs(local: &SyncCatalog, remote: &SyncCatalog) -> SyncCatalog {
    let mut deleted_hosts = local.deleted_hosts.clone();
    for (id, &ts) in &remote.deleted_hosts {
        deleted_hosts
            .entry(id.clone())
            .and_modify(|cur| *cur = (*cur).max(ts))
            .or_insert(ts);
    }

    // 保留本地顺序在前、远端新增追加在后；同 UUID 按 updated_at_ms 取较新者
    let mut order: Vec<String> = Vec::new();
    let mut by_id: HashMap<String, HostProfile> = HashMap::new();

    for h in &local.hosts {
        if !by_id.contains_key(&h.id) {
            order.push(h.id.clone());
        }
        by_id.insert(h.id.clone(), h.clone());
    }
    for rh in &remote.hosts {
        match by_id.get(&rh.id) {
            None => {
                order.push(rh.id.clone());
                by_id.insert(rh.id.clone(), rh.clone());
            }
            Some(lh) => {
                if rh.updated_at_ms > lh.updated_at_ms {
                    by_id.insert(rh.id.clone(), rh.clone());
                }
            }
        }
    }

    let mut hosts = Vec::with_capacity(order.len());
    for id in order {
        if let Some(h) = by_id.remove(&id) {
            let is_deleted = deleted_hosts
                .get(&h.id)
                .is_some_and(|&del_ms| del_ms >= h.updated_at_ms);
            if !is_deleted {
                hosts.push(h);
            }
        }
    }

    // 分组与分组删除墓碑合并
    let mut deleted_groups = local.deleted_groups.clone();
    for (g, &ts) in &remote.deleted_groups {
        deleted_groups
            .entry(g.clone())
            .and_modify(|cur| *cur = (*cur).max(ts))
            .or_insert(ts);
    }

    let mut group_updated_at = local.group_updated_at.clone();
    for (g, &ts) in &remote.group_updated_at {
        group_updated_at
            .entry(g.clone())
            .and_modify(|cur| *cur = (*cur).max(ts))
            .or_insert(ts);
    }
    for g in local.custom_groups.iter().chain(remote.custom_groups.iter()) {
        if !g.trim().is_empty() {
            group_updated_at.entry(g.clone()).or_insert(1);
        }
    }

    let mut group_order: Vec<String> = Vec::new();
    for g in local
        .custom_groups
        .iter()
        .chain(remote.custom_groups.iter())
        .chain(group_updated_at.keys())
    {
        let trimmed = g.trim();
        if !trimmed.is_empty() && !group_order.iter().any(|x| x == trimmed) {
            group_order.push(trimmed.to_string());
        }
    }

    let mut custom_groups = Vec::new();
    for g in group_order {
        let updated = group_updated_at.get(&g).copied().unwrap_or(1);
        let deleted = deleted_groups.get(&g).copied().unwrap_or(0);
        let used_by_host = hosts.iter().any(|h| h.group.trim() == g);
        if used_by_host || updated > deleted {
            custom_groups.push(g);
        }
    }

    SyncCatalog {
        hosts,
        deleted_hosts,
        custom_groups,
        group_updated_at,
        deleted_groups,
    }
}

/// 解封快照内的基础目录与所有并发锁定补丁并依次合并
pub fn unseal_snapshot_catalog(
    vault_data: &VaultData,
    snapshot: &EncryptedSyncSnapshot,
) -> Result<SyncCatalog, String> {
    let mut merged: SyncCatalog =
        vault::unseal_json_with_vault(vault_data, &snapshot.sealed_catalog)?;
    for patch in &snapshot.sealed_patches {
        let delta: SyncCatalog = vault::unseal_json_with_vault(vault_data, patch)?;
        merged = merge_catalogs(&merged, &delta);
    }
    Ok(merged)
}

/// 使用主密码解锁云端暂存快照并与本地未推送的凭据/目录无损合并
pub fn unlock_and_merge_snapshot(
    master_password: &str,
    snapshot: &EncryptedSyncSnapshot,
    local_catalog: &SyncCatalog,
    local_vault_file: Option<&EncryptedVaultFile>,
) -> Result<([u8; 32], VaultData, EncryptedVaultFile, SyncCatalog), String> {
    let (remote_key, mut merged_vault) =
        vault::unlock_encrypted_file(&snapshot.vault, master_password)?;
    let remote_catalog = unseal_snapshot_catalog(&merged_vault, snapshot)?;

    let merged_catalog = merge_catalogs(local_catalog, &remote_catalog);

    // 若本机已有密钥库且可用同一主密码解开，先解封合并本地未推送凭据，绝不丢失锁定态新写入密码
    if let Some(local_enc) = local_vault_file {
        if let Ok((_, local_vault)) = vault::unlock_encrypted_file(local_enc, master_password) {
            vault::merge_vault_secrets(&mut merged_vault, &local_vault, &merged_catalog.deleted_hosts);
        }
    } else {
        // 仅清理已被墓碑删除的主机凭据
        merged_vault.host_secrets.retain(|hid, entry| {
            merged_catalog
                .deleted_hosts
                .get(hid)
                .is_none_or(|&del_ms| del_ms < entry.updated_at_ms)
        });
    }

    let consolidated_vault_file =
        vault::encrypt_vault_with_template(&remote_key, &snapshot.vault, &merged_vault)?;
    vault::save_encrypted_file(&consolidated_vault_file)?;

    Ok((
        remote_key,
        merged_vault,
        consolidated_vault_file,
        merged_catalog,
    ))
}

/// 已解锁状态下，直接用当前内存密钥合并暂存的云端快照（返回 None 表示主密码/盐不同，需单独验证）
pub fn try_merge_snapshot_with_unlocked_key(
    unlocked_key: &[u8; 32],
    local_vault_data: &VaultData,
    local_catalog: &SyncCatalog,
    snapshot: &EncryptedSyncSnapshot,
) -> Option<(VaultData, EncryptedVaultFile, SyncCatalog)> {
    let remote_vault = vault::decrypt_vault_with_key(unlocked_key, &snapshot.vault).ok()?;
    let remote_catalog = unseal_snapshot_catalog(&remote_vault, snapshot).ok()?;

    let merged_catalog = merge_catalogs(local_catalog, &remote_catalog);
    let mut merged_vault = local_vault_data.clone();
    vault::merge_vault_secrets(&mut merged_vault, &remote_vault, &merged_catalog.deleted_hosts);

    let consolidated_vault_file =
        vault::encrypt_vault_with_template(unlocked_key, &snapshot.vault, &merged_vault).ok()?;
    vault::save_encrypted_file(&consolidated_vault_file).ok()?;

    Some((merged_vault, consolidated_vault_file, merged_catalog))
}

// ====================== 异步任务调度入口 ======================

/// 在后台 Tokio 线程池中异步执行云端同步操作
pub fn spawn_sync_task(req: SyncRequest, tx: mpsc::Sender<SyncEvent>) {
    runtime().spawn(async move {
        let res = run_sync(req).await;
        match res {
            Ok(ev) => {
                let _ = tx.send(ev);
            }
            Err(err) => {
                let _ = tx.send(SyncEvent::Error { message: err });
            }
        }
    });
}

async fn run_sync(req: SyncRequest) -> Result<SyncEvent, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .build()
        .map_err(|e| trf("svc.sync.client_err", &[("err", &e.to_string())]))?;

    if req.settings.sync_provider == SyncProvider::Disabled {
        return Err(tr("svc.sync.select_provider").into());
    }

    match req.action {
        SyncAction::TestConnection => run_test(&client, &req.settings).await,
        SyncAction::ProbeRemote => run_probe(&client, req).await,
        SyncAction::Push => run_push(&client, req, false).await,
        SyncAction::ForcePush => run_push(&client, req, true).await,
        SyncAction::Pull => run_pull(&client, req, false).await,
        SyncAction::AutoPull => run_pull(&client, req, true).await,
    }
}

/// 保存同步配置后立即探测云端是否已有快照
async fn run_probe(client: &reqwest::Client, req: SyncRequest) -> Result<SyncEvent, String> {
    let remote_opt = fetch_remote_snapshot(client, &req.settings).await?;
    let Some(snapshot) = remote_opt else {
        return Ok(SyncEvent::ProbeEmpty);
    };
    let same_vault_salt = req
        .local_vault_file
        .as_ref()
        .is_some_and(|lv| lv.salt_hex == snapshot.vault.salt_hex);
    Ok(SyncEvent::ConflictPrompt {
        reason: SyncConflictReason::InitialBind,
        snapshot,
        same_vault_salt,
    })
}

async fn run_test(client: &reqwest::Client, settings: &AppSettings) -> Result<SyncEvent, String> {
    match settings.sync_provider {
        SyncProvider::Disabled => Err(tr("svc.sync.select_provider").into()),
        SyncProvider::GitHubGist => test_gist(client, &settings.sync_gist_token).await,
        SyncProvider::S3 => test_s3(client, settings).await,
    }
}

/// Pull-Before-Push：推送前先拉取远端快照并校验 sync_epoch / 密钥库指纹；
/// 同源时自动无损合并，分叉时暂停并弹出决策弹窗；`force_overwrite=true` 则生成新 sync_epoch 强覆云端
async fn run_push(
    client: &reqwest::Client,
    req: SyncRequest,
    force_overwrite: bool,
) -> Result<SyncEvent, String> {
    let local_vault_file = req
        .local_vault_file
        .ok_or_else(|| tr("svc.sync.vault_required").to_string())?;
    if local_vault_file.write_public_key_hex.trim().is_empty() {
        return Err(tr("svc.sync.vault_upgrade_required").into());
    }

    let remote_opt = if force_overwrite {
        // 强制覆盖云端时容忍云端旧文件格式损坏，仍尝试读取 revision 以单调递增
        fetch_remote_snapshot(client, &req.settings)
            .await
            .ok()
            .flatten()
    } else {
        fetch_remote_snapshot(client, &req.settings).await?
    };

    let mut merged_catalog_out: Option<SyncCatalog> = None;
    let mut merged_vault_data_out: Option<VaultData> = None;
    let mut merged_vault_file_out: Option<EncryptedVaultFile> = None;

    let snapshot_to_upload = if force_overwrite {
        let next_rev = remote_opt
            .as_ref()
            .map(|r| r.revision.saturating_add(1))
            .unwrap_or_else(|| req.settings.sync_base_revision.saturating_add(1).max(1));
        let new_epoch = generate_host_uuid();
        let sealed_catalog = vault::seal_json_with_pubkey(
            &local_vault_file.write_public_key_hex,
            &req.local_catalog,
        )?;
        EncryptedSyncSnapshot {
            format: SYNC_SNAPSHOT_FORMAT.into(),
            revision: next_rev,
            sync_epoch: new_epoch,
            updated_at_ms: now_unix_ms(),
            vault: local_vault_file,
            sealed_catalog,
            sealed_patches: Vec::new(),
        }
    } else {
        match remote_opt {
            None => {
                let epoch = if req.settings.sync_epoch.is_empty() {
                    generate_host_uuid()
                } else {
                    req.settings.sync_epoch.clone()
                };
                let sealed_catalog = vault::seal_json_with_pubkey(
                    &local_vault_file.write_public_key_hex,
                    &req.local_catalog,
                )?;
                EncryptedSyncSnapshot {
                    format: SYNC_SNAPSHOT_FORMAT.into(),
                    revision: req.settings.sync_base_revision.saturating_add(1).max(1),
                    sync_epoch: epoch,
                    updated_at_ms: now_unix_ms(),
                    vault: local_vault_file,
                    sealed_catalog,
                    sealed_patches: Vec::new(),
                }
            }
            Some(mut remote) => {
                if let Some(reason) =
                    detect_sync_conflict(&req.settings, Some(&local_vault_file), &remote)
                {
                    let same_vault_salt = local_vault_file.salt_hex == remote.vault.salt_hex;
                    return Ok(SyncEvent::ConflictPrompt {
                        reason,
                        snapshot: remote,
                        same_vault_salt,
                    });
                }

                let epoch = if !remote.sync_epoch.is_empty() {
                    remote.sync_epoch.clone()
                } else if !req.settings.sync_epoch.is_empty() {
                    req.settings.sync_epoch.clone()
                } else {
                    generate_host_uuid()
                };
                let next_rev = remote.revision.saturating_add(1);
                if let (Some(key), Some(local_vdata)) =
                    (req.unlocked_key.as_ref(), req.unlocked_vault_data.as_ref())
                {
                    if remote.vault.salt_hex == local_vault_file.salt_hex {
                        // 同盐同密钥：先解封合并远端全部目录、补丁与密钥，再整体压缩推送
                        let remote_vdata = vault::decrypt_vault_with_key(key, &remote.vault)?;
                        let remote_cat = unseal_snapshot_catalog(&remote_vdata, &remote)?;
                        let merged_cat = merge_catalogs(&req.local_catalog, &remote_cat);
                        let mut merged_vdata = local_vdata.clone();
                        vault::merge_vault_secrets(
                            &mut merged_vdata,
                            &remote_vdata,
                            &merged_cat.deleted_hosts,
                        );
                        let fresh_vault_file = vault::encrypt_vault_with_template(
                            key,
                            &local_vault_file,
                            &merged_vdata,
                        )?;
                        let sealed_catalog = vault::seal_json_with_pubkey(
                            &fresh_vault_file.write_public_key_hex,
                            &merged_cat,
                        )?;
                        merged_catalog_out = Some(merged_cat);
                        merged_vault_data_out = Some(merged_vdata);
                        merged_vault_file_out = Some(fresh_vault_file.clone());
                        EncryptedSyncSnapshot {
                            format: SYNC_SNAPSHOT_FORMAT.into(),
                            revision: next_rev,
                            sync_epoch: epoch,
                            updated_at_ms: now_unix_ms(),
                            vault: fresh_vault_file,
                            sealed_catalog,
                            sealed_patches: Vec::new(),
                        }
                    } else if remote.revision <= req.settings.sync_base_revision {
                        // 本地刚修改主密码导致新盐且远端无并发变更：开启新纪元安全上传
                        let sealed_catalog = vault::seal_json_with_pubkey(
                            &local_vault_file.write_public_key_hex,
                            &req.local_catalog,
                        )?;
                        EncryptedSyncSnapshot {
                            format: SYNC_SNAPSHOT_FORMAT.into(),
                            revision: next_rev,
                            sync_epoch: generate_host_uuid(),
                            updated_at_ms: now_unix_ms(),
                            vault: local_vault_file,
                            sealed_catalog,
                            sealed_patches: Vec::new(),
                        }
                    } else {
                        return Ok(SyncEvent::ConflictPrompt {
                            reason: SyncConflictReason::RemoteDiverged,
                            snapshot: remote,
                            same_vault_salt: false,
                        });
                    }
                } else if remote.revision <= req.settings.sync_base_revision {
                    // 锁定态且远端没有更新版本：直接以 X25519 公钥封存全量目录上传
                    let sealed_catalog = vault::seal_json_with_pubkey(
                        &local_vault_file.write_public_key_hex,
                        &req.local_catalog,
                    )?;
                    EncryptedSyncSnapshot {
                        format: SYNC_SNAPSHOT_FORMAT.into(),
                        revision: next_rev,
                        sync_epoch: epoch,
                        updated_at_ms: now_unix_ms(),
                        vault: local_vault_file,
                        sealed_catalog,
                        sealed_patches: Vec::new(),
                    }
                } else if vault::has_same_write_pubkey(&remote.vault, &local_vault_file) {
                    // 锁定态且远端已被其他设备更新（同密钥库）：保留远端密文，仅追加本地封存增量补丁与封存凭据！
                    for (hid, sealed_entry) in local_vault_file.sealed_entries {
                        remote.vault.sealed_entries.insert(hid, sealed_entry);
                    }
                    for del_id in local_vault_file.pending_deletes {
                        remote.vault.sealed_entries.remove(&del_id);
                        if !remote.vault.pending_deletes.iter().any(|x| x == &del_id) {
                            remote.vault.pending_deletes.push(del_id);
                        }
                    }
                    let patch = vault::seal_json_with_pubkey(
                        &remote.vault.write_public_key_hex,
                        &req.local_catalog,
                    )?;
                    remote.sealed_patches.push(patch);
                    remote.revision = next_rev;
                    remote.sync_epoch = epoch;
                    remote.updated_at_ms = now_unix_ms();
                    remote
                } else {
                    return Ok(SyncEvent::ConflictPrompt {
                        reason: SyncConflictReason::RemoteDiverged,
                        snapshot: remote,
                        same_vault_salt: false,
                    });
                }
            }
        }
    };

    let host_count = merged_catalog_out
        .as_ref()
        .map(|c| c.hosts.len())
        .unwrap_or(req.local_catalog.hosts.len());
    let revision = snapshot_to_upload.revision;
    let sync_epoch = snapshot_to_upload.sync_epoch.clone();
    let vault_salt = snapshot_to_upload.vault.salt_hex.clone();
    let body = serde_json::to_string_pretty(&snapshot_to_upload)
        .map_err(|e| trf("svc.sync.serialize_err", &[("err", &e.to_string())]))?;

    let (created_gist_id, message, synced_at) = match req.settings.sync_provider {
        SyncProvider::GitHubGist => {
            upload_gist(client, &req.settings, &body, host_count, revision).await?
        }
        SyncProvider::S3 => upload_s3(client, &req.settings, &body, host_count, revision).await?,
        SyncProvider::Disabled => return Err(tr("svc.sync.select_provider").into()),
    };

    Ok(SyncEvent::PushOk {
        message,
        created_gist_id,
        revision,
        sync_epoch,
        vault_salt,
        synced_at,
        merged_catalog: merged_catalog_out,
        merged_vault_data: merged_vault_data_out,
        merged_vault_file: merged_vault_file_out,
    })
}

/// 安全拉取：先校验 sync_epoch / 密钥库指纹（分叉立即弹窗不静默覆盖）；同源且已解锁直接合并，锁定态暂存快照
async fn run_pull(
    client: &reqwest::Client,
    req: SyncRequest,
    is_auto_pull: bool,
) -> Result<SyncEvent, String> {
    let Some(snapshot) = fetch_remote_snapshot(client, &req.settings).await? else {
        if is_auto_pull {
            return Ok(SyncEvent::PullUpToDate);
        }
        return Err(tr("svc.sync.remote_not_found").into());
    };

    // 关键：在比较 revision 之前先校验 sync_epoch 与 vault salt，防止强覆后 revision 较小被误判为最新
    if let Some(reason) =
        detect_sync_conflict(&req.settings, req.local_vault_file.as_ref(), &snapshot)
    {
        let same_vault_salt = req
            .local_vault_file
            .as_ref()
            .is_some_and(|lv| lv.salt_hex == snapshot.vault.salt_hex);
        return Ok(SyncEvent::ConflictPrompt {
            reason,
            snapshot,
            same_vault_salt,
        });
    }

    if is_auto_pull && snapshot.revision <= req.settings.sync_base_revision {
        return Ok(SyncEvent::PullUpToDate);
    }

    let time_s = now_local_display();
    let rev_s = snapshot.revision.to_string();
    let effective_epoch = if !snapshot.sync_epoch.is_empty() {
        snapshot.sync_epoch.clone()
    } else {
        req.settings.sync_epoch.clone()
    };
    let effective_salt = snapshot.vault.salt_hex.clone();

    if let (Some(key), Some(local_vdata)) =
        (req.unlocked_key.as_ref(), req.unlocked_vault_data.as_ref())
    {
        if let Some((merged_vdata, merged_vfile, merged_cat)) =
            try_merge_snapshot_with_unlocked_key(key, local_vdata, &req.local_catalog, &snapshot)
        {
            let count_s = merged_cat.hosts.len().to_string();
            let synced_at = match req.settings.sync_provider {
                SyncProvider::GitHubGist => trf(
                    "svc.sync.gist_pulled_at",
                    &[("time", &time_s), ("count", &count_s)],
                ),
                _ => trf(
                    "svc.sync.s3_pulled_at",
                    &[("time", &time_s), ("count", &count_s)],
                ),
            };
            return Ok(SyncEvent::PullMerged {
                revision: snapshot.revision,
                sync_epoch: effective_epoch,
                vault_salt: effective_salt,
                synced_at,
                merged_catalog: merged_cat,
                merged_vault_data: merged_vdata,
                merged_vault_file: merged_vfile,
            });
        }
    }

    let synced_at = trf(
        "svc.sync.pulled_rev_at",
        &[("time", &time_s), ("rev", &rev_s)],
    );
    let message = trf("svc.sync.pull_needs_unlock", &[("rev", &rev_s)]);
    Ok(SyncEvent::PullNeedsUnlock {
        message,
        snapshot,
        synced_at,
        is_auto_pull,
    })
}

async fn fetch_remote_snapshot(
    client: &reqwest::Client,
    settings: &AppSettings,
) -> Result<Option<EncryptedSyncSnapshot>, String> {
    let raw_opt = match settings.sync_provider {
        SyncProvider::Disabled => return Err(tr("svc.sync.select_provider").into()),
        SyncProvider::GitHubGist => fetch_gist_raw(client, settings).await?,
        SyncProvider::S3 => fetch_s3_raw(client, settings).await?,
    };
    let Some(raw) = raw_opt else {
        return Ok(None);
    };
    let snap = parse_encrypted_snapshot(&raw)?;
    Ok(Some(snap))
}

pub fn parse_encrypted_snapshot(raw: &str) -> Result<EncryptedSyncSnapshot, String> {
    let snap: EncryptedSyncSnapshot = serde_json::from_str(raw)
        .map_err(|e| trf("svc.sync.parse_err", &[("err", &e.to_string())]))?;
    if snap.format != SYNC_SNAPSHOT_FORMAT {
        return Err(tr("svc.sync.invalid_format").into());
    }
    Ok(snap)
}

fn now_local_display() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

// ====================== GitHub Gist 传输实现（含 >1MB raw_url 支持） ======================

fn gist_headers(token: &str) -> Result<HeaderMap, String> {
    let tok = token.trim();
    if tok.is_empty() {
        return Err(tr("svc.sync.gist_token_req").into());
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(concat!("Holossh/", env!("CARGO_PKG_VERSION"))),
    );
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let auth = HeaderValue::from_str(&format!("Bearer {tok}"))
        .map_err(|_| tr("svc.sync.gist_token_invalid").to_string())?;
    headers.insert(AUTHORIZATION, auth);
    Ok(headers)
}

fn effective_gist_filename(settings: &AppSettings) -> &str {
    let name = settings.gist_filename.trim();
    if name.is_empty() {
        "holossh-sync.json"
    } else {
        name
    }
}

async fn test_gist(client: &reqwest::Client, token: &str) -> Result<SyncEvent, String> {
    let headers = gist_headers(token)?;
    let resp = client
        .get("https://api.github.com/user")
        .headers(headers)
        .send()
        .await
        .map_err(|e| trf("svc.sync.gist_conn_err", &[("err", &e.to_string())]))?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(trf(
            "svc.sync.gist_auth_err",
            &[("code", &status.as_u16().to_string())],
        ));
    }
    let login = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("login").and_then(|l| l.as_str()).map(str::to_string))
        .unwrap_or_else(|| tr("svc.sync.auth_user").into());

    Ok(SyncEvent::TestOk {
        message: trf("svc.sync.gist_test_ok", &[("login", &login)]),
    })
}

async fn fetch_gist_raw(
    client: &reqwest::Client,
    settings: &AppSettings,
) -> Result<Option<String>, String> {
    let gist_id = settings.gist_id.trim();
    if gist_id.is_empty() {
        return Ok(None);
    }
    let headers = gist_headers(&settings.sync_gist_token)?;
    let resp = client
        .get(format!("https://api.github.com/gists/{gist_id}"))
        .headers(headers.clone())
        .send()
        .await
        .map_err(|e| trf("svc.sync.gist_pull_err", &[("err", &e.to_string())]))?;

    let status = resp.status();
    if status.as_u16() == 404 {
        return Ok(None);
    }
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(trf(
            "svc.sync.gist_read_http_err",
            &[
                ("code", &status.as_u16().to_string()),
                ("detail", &extract_error_snippet(&text)),
            ],
        ));
    }

    let val: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| trf("svc.sync.gist_parse_err", &[("err", &e.to_string())]))?;
    let files = val
        .get("files")
        .and_then(|f| f.as_object())
        .ok_or_else(|| tr("svc.sync.gist_no_files").to_string())?;

    let target_name = effective_gist_filename(settings);
    let Some(file_entry) = files.get(target_name).or_else(|| files.values().next()) else {
        return Ok(None);
    };

    // 处理 GitHub Gist > 1MB 文件自动截断：通过 raw_url 下载完整密文
    let is_truncated = file_entry
        .get("truncated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if is_truncated {
        let raw_url = file_entry
            .get("raw_url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| tr("svc.sync.gist_file_empty").to_string())?;
        let raw_resp = client
            .get(raw_url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| trf("svc.sync.gist_pull_err", &[("err", &e.to_string())]))?;
        let r_status = raw_resp.status();
        let r_text = raw_resp.text().await.unwrap_or_default();
        if !r_status.is_success() {
            return Err(trf(
                "svc.sync.gist_read_http_err",
                &[
                    ("code", &r_status.as_u16().to_string()),
                    ("detail", &extract_error_snippet(&r_text)),
                ],
            ));
        }
        return Ok(Some(r_text));
    }

    let content = file_entry
        .get("content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| tr("svc.sync.gist_file_empty").to_string())?;
    Ok(Some(content.to_string()))
}

async fn upload_gist(
    client: &reqwest::Client,
    settings: &AppSettings,
    content: &str,
    host_count: usize,
    revision: u64,
) -> Result<(Option<String>, String, String), String> {
    let headers = gist_headers(&settings.sync_gist_token)?;
    let filename = effective_gist_filename(settings);

    let mut files = serde_json::Map::new();
    let mut file_obj = serde_json::Map::new();
    file_obj.insert("content".into(), serde_json::Value::String(content.to_string()));
    files.insert(filename.to_string(), serde_json::Value::Object(file_obj));

    let gist_id = settings.gist_id.trim();
    let (resp, creating) = if gist_id.is_empty() {
        let body = serde_json::json!({
            "description": "Holossh End-to-End Encrypted Sync Snapshot",
            "public": false,
            "files": files,
        });
        let r = client
            .post("https://api.github.com/gists")
            .headers(headers)
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| trf("svc.sync.gist_create_err", &[("err", &e.to_string())]))?;
        (r, true)
    } else {
        let body = serde_json::json!({
            "description": "Holossh End-to-End Encrypted Sync Snapshot",
            "files": files,
        });
        let r = client
            .patch(format!("https://api.github.com/gists/{gist_id}"))
            .headers(headers)
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| trf("svc.sync.gist_update_err", &[("err", &e.to_string())]))?;
        (r, false)
    };

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(trf(
            "svc.sync.gist_push_http_err",
            &[
                ("code", &status.as_u16().to_string()),
                ("detail", &extract_error_snippet(&text)),
            ],
        ));
    }

    let returned_id = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_string));

    let count_s = host_count.to_string();
    let rev_s = revision.to_string();
    let stamp = trf(
        "svc.sync.gist_pushed_at",
        &[("time", &now_local_display()), ("count", &count_s)],
    );
    let msg = if creating {
        trf(
            "svc.sync.gist_created_ok",
            &[("count", &count_s), ("vault", &format!(" (v{rev_s})"))],
        )
    } else {
        trf(
            "svc.sync.gist_pushed_ok",
            &[("count", &count_s), ("vault", &format!(" (v{rev_s})"))],
        )
    };
    Ok((if creating { returned_id } else { None }, msg, stamp))
}

// ====================== S3 兼容对象存储（AWS SigV4 + 自动虚拟主机风格 + 路径编码） ======================

struct S3Target {
    url: Url,
    region: String,
    access_key: String,
    secret_key: String,
}

/// 判断是否应采用 Virtual-Hosted Style（`https://{bucket}.{host}/{key}`）：
/// - 阿里云 OSS / 腾讯云 COS / 华为云 OBS / 火山引擎 TOS / AWS S3 标准域名采用虚拟主机风格；
/// - IP、localhost、带自定义端口、MinIO、Cloudflare R2 采用 Path-Style。
fn should_use_virtual_hosted(endpoint_url: &Url, bucket: &str) -> bool {
    if endpoint_url.port().is_some() {
        return false;
    }
    let Some(host) = endpoint_url.host_str() else {
        return false;
    };
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.parse::<IpAddr>().is_ok() {
        return false;
    }
    if lower.ends_with(".r2.cloudflarestorage.com") {
        return false;
    }
    // 桶名必须符合 DNS 标签规范且不包含点号（避免 TLS 泛域名证书校验失败）
    if bucket.contains('.') {
        return false;
    }
    lower.ends_with(".aliyuncs.com")
        || lower.ends_with(".myqcloud.com")
        || lower.ends_with(".myhuaweicloud.com")
        || lower.ends_with(".amazonaws.com")
        || lower.ends_with(".volces.com")
}

/// 按 AWS SigV4 规范对 URI 路径做分片百分号编码（保留 `/` 与未保留字符 `A-Za-z0-9-_.~`）
pub fn aws_uri_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for (idx, segment) in path.split('/').enumerate() {
        if idx > 0 {
            out.push('/');
        }
        for &b in segment.as_bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    if out.is_empty() {
        "/".into()
    } else if !out.starts_with('/') {
        format!("/{out}")
    } else {
        out
    }
}

pub fn build_s3_object_url(endpoint: &str, bucket: &str, object_key: &str) -> Result<Url, String> {
    let ep_raw = endpoint.trim().trim_end_matches('/');
    let bucket_clean = bucket.trim().trim_matches('/');
    let key_clean = object_key.trim().trim_start_matches('/');

    let ep_url = Url::parse(ep_raw)
        .map_err(|e| trf("svc.sync.s3_url_invalid", &[("url", ep_raw), ("err", &e.to_string())]))?;
    let host = ep_url.host_str().unwrap_or("");
    let encoded_key = aws_uri_encode_path(key_clean);
    let prefix_with_dot = format!("{bucket_clean}.");

    let full_url = if host
        .to_ascii_lowercase()
        .starts_with(&prefix_with_dot.to_ascii_lowercase())
    {
        // 用户已在 Endpoint 中自带 `{bucket}.` 前缀，避免重复拼接
        format!("{ep_raw}{encoded_key}")
    } else if should_use_virtual_hosted(&ep_url, bucket_clean) {
        let scheme = ep_url.scheme();
        format!("{scheme}://{bucket_clean}.{host}{encoded_key}")
    } else {
        let encoded_bucket = aws_uri_encode_path(bucket_clean);
        format!("{ep_raw}{encoded_bucket}{encoded_key}")
    };

    Url::parse(&full_url)
        .map_err(|e| trf("svc.sync.s3_url_invalid", &[("url", &full_url), ("err", &e.to_string())]))
}

fn resolve_s3_target(settings: &AppSettings) -> Result<S3Target, String> {
    let endpoint = settings.s3_endpoint.trim();
    let bucket = settings.s3_bucket.trim();
    let key = settings.s3_object_key.trim();
    let region = if settings.s3_region.trim().is_empty() {
        "us-east-1"
    } else {
        settings.s3_region.trim()
    };
    let access_key = settings.s3_access_key_id.trim();
    let secret = settings.sync_s3_secret.trim();

    if endpoint.is_empty() {
        return Err(tr("svc.sync.s3_endpoint_req").into());
    }
    if bucket.is_empty() {
        return Err(tr("svc.sync.s3_bucket_req").into());
    }
    if key.is_empty() {
        return Err(tr("svc.sync.s3_key_req").into());
    }
    if access_key.is_empty() || secret.is_empty() {
        return Err(tr("svc.sync.s3_creds_req").into());
    }

    let url = build_s3_object_url(endpoint, bucket, key)?;
    Ok(S3Target {
        url,
        region: region.to_string(),
        access_key: access_key.to_string(),
        secret_key: secret.to_string(),
    })
}

/// 标准 AWS Signature Version 4 请求签名器
pub fn sign_s3_request(
    method: &str,
    url: &Url,
    region: &str,
    access_key: &str,
    secret_key: &str,
    payload: &[u8],
    amz_date: &str,
    date_stamp: &str,
    content_type: Option<&str>,
) -> Result<HeaderMap, String> {
    let host = match url.port() {
        Some(p) => format!("{}:{p}", url.host_str().unwrap_or("")),
        None => url.host_str().unwrap_or("").to_string(),
    };
    let payload_hash = hex::encode(Sha256::digest(payload));

    let mut canonical_headers_map: BTreeMap<String, String> = BTreeMap::new();
    canonical_headers_map.insert("host".into(), host);
    canonical_headers_map.insert("x-amz-content-sha256".into(), payload_hash.clone());
    canonical_headers_map.insert("x-amz-date".into(), amz_date.to_string());
    if let Some(ct) = content_type {
        canonical_headers_map.insert("content-type".into(), ct.to_string());
    }

    let mut canonical_headers = String::new();
    let mut signed_header_keys = Vec::new();
    for (k, v) in &canonical_headers_map {
        canonical_headers.push_str(k);
        canonical_headers.push(':');
        canonical_headers.push_str(v.trim());
        canonical_headers.push('\n');
        signed_header_keys.push(k.as_str());
    }
    let signed_headers = signed_header_keys.join(";");

    let canonical_uri = if url.path().is_empty() { "/" } else { url.path() };
    let canonical_query = url.query().unwrap_or("");
    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );
    let canonical_request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));

    let credential_scope = format!("{date_stamp}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{canonical_request_hash}"
    );

    let k_date = hmac_sha256(format!("AWS4{secret_key}").as_bytes(), date_stamp.as_bytes())?;
    let k_region = hmac_sha256(&k_date, region.as_bytes())?;
    let k_service = hmac_sha256(&k_region, b"s3")?;
    let k_signing = hmac_sha256(&k_service, b"aws4_request")?;
    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes())?);

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}"
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("x-amz-date"),
        HeaderValue::from_str(amz_date).map_err(|_| tr("svc.sync.s3_header_err").to_string())?,
    );
    headers.insert(
        HeaderName::from_static("x-amz-content-sha256"),
        HeaderValue::from_str(&payload_hash)
            .map_err(|_| tr("svc.sync.s3_header_err").to_string())?,
    );
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&authorization)
            .map_err(|_| tr("svc.sync.s3_header_err").to_string())?,
    );
    if let Some(ct) = content_type {
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_str(ct).map_err(|_| tr("svc.sync.s3_header_err").to_string())?,
        );
    }
    Ok(headers)
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|e| trf("svc.sync.s3_hmac_err", &[("err", &e.to_string())]))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

async fn test_s3(client: &reqwest::Client, settings: &AppSettings) -> Result<SyncEvent, String> {
    let target = resolve_s3_target(settings)?;
    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let headers = sign_s3_request(
        "HEAD",
        &target.url,
        &target.region,
        &target.access_key,
        &target.secret_key,
        b"",
        &amz_date,
        &date_stamp,
        None,
    )?;

    let resp = client
        .head(target.url.clone())
        .headers(headers)
        .send()
        .await
        .map_err(|e| trf("svc.sync.s3_conn_err", &[("err", &e.to_string())]))?;

    let code = resp.status().as_u16();
    if code == 200 {
        Ok(SyncEvent::TestOk {
            message: trf(
                "svc.sync.s3_test_ok_exists",
                &[("target", &settings.s3_object_key)],
            ),
        })
    } else if code == 404 {
        Ok(SyncEvent::TestOk {
            message: tr("svc.sync.s3_test_ok_empty").into(),
        })
    } else {
        Err(trf(
            "svc.sync.s3_test_http_err",
            &[("code", &code.to_string())],
        ))
    }
}

async fn fetch_s3_raw(
    client: &reqwest::Client,
    settings: &AppSettings,
) -> Result<Option<String>, String> {
    let target = resolve_s3_target(settings)?;
    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let headers = sign_s3_request(
        "GET",
        &target.url,
        &target.region,
        &target.access_key,
        &target.secret_key,
        b"",
        &amz_date,
        &date_stamp,
        None,
    )?;

    let resp = client
        .get(target.url.clone())
        .headers(headers)
        .send()
        .await
        .map_err(|e| trf("svc.sync.s3_pull_err", &[("err", &e.to_string())]))?;

    let status = resp.status();
    if status.as_u16() == 404 {
        return Ok(None);
    }
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(trf(
            "svc.sync.s3_pull_http_err",
            &[
                ("code", &status.as_u16().to_string()),
                ("detail", &extract_error_snippet(&text)),
            ],
        ));
    }
    Ok(Some(text))
}

async fn upload_s3(
    client: &reqwest::Client,
    settings: &AppSettings,
    body: &str,
    host_count: usize,
    revision: u64,
) -> Result<(Option<String>, String, String), String> {
    let target = resolve_s3_target(settings)?;
    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let headers = sign_s3_request(
        "PUT",
        &target.url,
        &target.region,
        &target.access_key,
        &target.secret_key,
        body.as_bytes(),
        &amz_date,
        &date_stamp,
        Some("application/json"),
    )?;

    let resp = client
        .put(target.url.clone())
        .headers(headers)
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| trf("svc.sync.s3_push_err", &[("err", &e.to_string())]))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(trf(
            "svc.sync.s3_push_http_err",
            &[
                ("code", &status.as_u16().to_string()),
                ("detail", &extract_error_snippet(&text)),
            ],
        ));
    }

    let count_s = host_count.to_string();
    let rev_s = revision.to_string();
    let stamp = trf(
        "svc.sync.s3_pushed_at",
        &[("time", &now_local_display()), ("count", &count_s)],
    );
    let msg = trf(
        "svc.sync.s3_pushed_ok",
        &[
            ("count", &count_s),
            ("vault", &format!(" (v{rev_s})")),
            ("bucket", &settings.s3_bucket),
            ("key", &settings.s3_object_key),
        ],
    );
    Ok((None, msg, stamp))
}

fn extract_error_snippet(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return tr("svc.sync.no_detail").into();
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(msg) = v.get("message").and_then(|m| m.as_str()) {
            return msg.to_string();
        }
    }
    trimmed.chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::host::{AuthMethod, ConnectionKind, HostStatus, generate_host_uuid};

    fn sample_host(id: &str, name: &str, ip: &str, updated_at_ms: u64) -> HostProfile {
        HostProfile {
            id: id.into(),
            name: name.into(),
            group: "prod".into(),
            connection: ConnectionKind::Direct,
            jump_host_id: None,
            proxy_host: None,
            proxy_port: None,
            hostname: ip.into(),
            port: 22,
            username: "root".into(),
            auth_method: AuthMethod::Password,
            private_key_path: None,
            created_at_ms: updated_at_ms,
            updated_at_ms,
            status: HostStatus::Idle,
        }
    }

    #[test]
    fn crdt_merge_handles_concurrent_edits_and_tombstones() {
        let id_a = generate_host_uuid();
        let id_b = generate_host_uuid();
        let id_del = generate_host_uuid();
        assert_ne!(id_a, id_b);
        assert_eq!(id_a.len(), 36);

        let local = SyncCatalog {
            hosts: vec![
                sample_host(&id_a, "api-v2", "10.0.0.1", 2000),
                sample_host(&id_del, "old-box", "10.0.0.9", 1000),
            ],
            deleted_hosts: BTreeMap::new(),
            custom_groups: vec!["prod".into(), "empty-local".into()],
            group_updated_at: BTreeMap::from([
                ("prod".into(), 1000),
                ("empty-local".into(), 2000),
                ("removed-grp".into(), 1000),
            ]),
            deleted_groups: BTreeMap::new(),
        };

        let remote = SyncCatalog {
            hosts: vec![
                sample_host(&id_a, "api-v1", "10.0.0.1", 1500),
                sample_host(&id_b, "db-node", "10.0.0.2", 1800),
            ],
            deleted_hosts: BTreeMap::from([(id_del.clone(), 2500)]),
            custom_groups: vec!["prod".into()],
            group_updated_at: BTreeMap::from([("prod".into(), 1000)]),
            deleted_groups: BTreeMap::from([("removed-grp".into(), 3000)]),
        };

        let merged = merge_catalogs(&local, &remote);
        assert_eq!(merged.hosts.len(), 2);
        assert_eq!(merged.hosts[0].id, id_a);
        assert_eq!(merged.hosts[0].name, "api-v2"); // 较新时间戳胜出
        assert_eq!(merged.hosts[1].id, id_b); // 远端新增保留
        assert!(!merged.hosts.iter().any(|h| h.id == id_del)); // 删除墓碑生效不诈尸
        assert!(merged.custom_groups.contains(&"empty-local".to_string()));
        assert!(!merged.custom_groups.contains(&"removed-grp".to_string()));
    }

    #[test]
    fn inflight_stale_sync_snapshot_never_resurrects_concurrent_local_delete() {
        let id_keep = generate_host_uuid();
        let id_del = generate_host_uuid();

        // 模拟启动自动拉取或上一次推送在删除前捕获的过期合并快照（仍含「测试」分组和待删主机）
        let stale_incoming = SyncCatalog {
            hosts: vec![
                HostProfile {
                    group: "测试".into(),
                    ..sample_host(&id_keep, "node-a", "10.0.0.1", 1000)
                },
                sample_host(&id_del, "node-del", "10.0.0.2", 1000),
            ],
            deleted_hosts: BTreeMap::new(),
            custom_groups: vec!["测试".into(), "测试 2".into()],
            group_updated_at: BTreeMap::from([("测试".into(), 1000), ("测试 2".into(), 1000)]),
            deleted_groups: BTreeMap::new(),
        };

        // 在同步进行期间，用户在侧边栏确认删除了「测试」「测试 2」分组并删除了 node-del
        let live_local = SyncCatalog {
            hosts: vec![HostProfile {
                group: String::new(),
                ..sample_host(&id_keep, "node-a", "10.0.0.1", 2000)
            }],
            deleted_hosts: BTreeMap::from([(id_del.clone(), 2000)]),
            custom_groups: Vec::new(),
            group_updated_at: BTreeMap::new(),
            deleted_groups: BTreeMap::from([("测试".into(), 2000), ("测试 2".into(), 2000)]),
        };

        let merged = merge_catalogs(&live_local, &stale_incoming);
        assert_eq!(merged.hosts.len(), 1);
        assert_eq!(merged.hosts[0].id, id_keep);
        assert!(merged.hosts[0].group.is_empty());
        assert!(merged.custom_groups.is_empty());
        assert_eq!(merged.deleted_groups.get("测试"), Some(&2000));
        assert_eq!(merged.deleted_groups.get("测试 2"), Some(&2000));
        assert_eq!(merged.deleted_hosts.get(&id_del), Some(&2000));
    }

    #[test]
    fn s3_url_style_and_sigv4_encoding() {
        // 阿里云 OSS 自动 Virtual-Hosted Style + 中文 Key 百分号编码
        let oss = build_s3_object_url(
            "https://oss-cn-hangzhou.aliyuncs.com",
            "my-bucket",
            "holossh/同步 快照.json",
        )
        .unwrap();
        assert_eq!(
            oss.as_str(),
            "https://my-bucket.oss-cn-hangzhou.aliyuncs.com/holossh/%E5%90%8C%E6%AD%A5%20%E5%BF%AB%E7%85%A7.json"
        );

        // Endpoint 已带 bucket 前缀不重复拼接
        let oss_prefixed = build_s3_object_url(
            "https://my-bucket.oss-cn-hangzhou.aliyuncs.com",
            "my-bucket",
            "holossh/sync.json",
        )
        .unwrap();
        assert_eq!(
            oss_prefixed.as_str(),
            "https://my-bucket.oss-cn-hangzhou.aliyuncs.com/holossh/sync.json"
        );

        // MinIO / Cloudflare R2 自动 Path-Style
        let r2 = build_s3_object_url(
            "https://abc123.r2.cloudflarestorage.com",
            "vault-bucket",
            "holossh/sync.json",
        )
        .unwrap();
        assert_eq!(
            r2.as_str(),
            "https://abc123.r2.cloudflarestorage.com/vault-bucket/holossh/sync.json"
        );

        let headers = sign_s3_request(
            "PUT",
            &oss,
            "oss-cn-hangzhou",
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            b"{}",
            "20261122T085000Z",
            "20261122",
            Some("application/json"),
        )
        .unwrap();
        let auth = headers.get(AUTHORIZATION).unwrap().to_str().unwrap();
        assert!(auth.contains("Signature="));
    }

    #[test]
    fn full_e2ee_snapshot_hides_all_host_metadata_and_unseals_patches() {
        let mut salt = [0u8; 16];
        getrandom::fill(&mut salt).unwrap();
        // 复用 vault 公开接口：通过临时创建内存加密信封验证全量加密与补丁合并
        let temp_vault = EncryptedVaultFile {
            version: 1,
            kdf: "argon2id".into(),
            m_cost: 8192,
            t_cost: 1,
            p_cost: 1,
            salt_hex: hex::encode(salt),
            nonce_hex: String::new(),
            ciphertext_hex: String::new(),
            write_public_key_hex: String::new(),
            sealed_entries: HashMap::new(),
            pending_deletes: Vec::new(),
        };
        let key = [42u8; 32];
        let vdata = VaultData::default();
        let vfile = vault::encrypt_vault_with_template(&key, &temp_vault, &vdata).unwrap();
        let vdata_unlocked = vault::decrypt_vault_with_key(&key, &vfile).unwrap();

        let id1 = generate_host_uuid();
        let id2 = generate_host_uuid();
        let cat1 = SyncCatalog {
            hosts: vec![sample_host(&id1, "secret-prod-node", "203.0.113.42", 1000)],
            ..SyncCatalog::default()
        };
        let cat2 = SyncCatalog {
            hosts: vec![sample_host(&id2, "secret-bastion", "198.51.100.77", 2000)],
            ..SyncCatalog::default()
        };

        let sealed_base = vault::seal_json_with_pubkey(&vfile.write_public_key_hex, &cat1).unwrap();
        let sealed_patch =
            vault::seal_json_with_pubkey(&vfile.write_public_key_hex, &cat2).unwrap();

        let snap = EncryptedSyncSnapshot {
            format: SYNC_SNAPSHOT_FORMAT.into(),
            revision: 2,
            sync_epoch: "epoch-alpha".into(),
            updated_at_ms: 2000,
            vault: vfile,
            sealed_catalog: sealed_base,
            sealed_patches: vec![sealed_patch],
        };
        let raw_json = serde_json::to_string(&snap).unwrap();
        // 云端密文绝不能包含任何明文主机名或 IP
        assert!(!raw_json.contains("secret-prod-node"));
        assert!(!raw_json.contains("203.0.113.42"));
        assert!(!raw_json.contains("secret-bastion"));
        assert!(!raw_json.contains("198.51.100.77"));

        let parsed = parse_encrypted_snapshot(&raw_json).unwrap();
        let merged = unseal_snapshot_catalog(&vdata_unlocked, &parsed).unwrap();
        assert_eq!(merged.hosts.len(), 2);
        assert!(merged.hosts.iter().any(|h| h.hostname == "203.0.113.42"));
        assert!(merged.hosts.iter().any(|h| h.hostname == "198.51.100.77"));
    }

    #[test]
    fn sync_epoch_and_vault_salt_divergence_detection() {
        let mut settings = AppSettings::default();
        let make_snap = |epoch: &str, salt: &str, rev: u64| EncryptedSyncSnapshot {
            format: SYNC_SNAPSHOT_FORMAT.into(),
            revision: rev,
            sync_epoch: epoch.into(),
            updated_at_ms: 1_700_000_000_000,
            vault: EncryptedVaultFile {
                version: 1,
                kdf: "argon2id".into(),
                m_cost: 8192,
                t_cost: 1,
                p_cost: 1,
                salt_hex: salt.into(),
                nonce_hex: String::new(),
                ciphertext_hex: String::new(),
                write_public_key_hex: String::new(),
                sealed_entries: HashMap::new(),
                pending_deletes: Vec::new(),
            },
            sealed_catalog: SealedBlob {
                eph_pubkey_hex: String::new(),
                nonce_hex: String::new(),
                ciphertext_hex: String::new(),
            },
            sealed_patches: Vec::new(),
        };

        // 1. 首次未绑定（sync_epoch 为空）遇到云端快照 → 触发 InitialBind
        let snap1 = make_snap("epoch-1", "salt-a", 5);
        assert_eq!(
            detect_sync_conflict(&settings, None, &snap1),
            Some(SyncConflictReason::InitialBind)
        );

        // 2. 已绑定同 epoch & 同 salt → 正常无冲突，静默 CRDT 合并
        settings.sync_epoch = "epoch-1".into();
        settings.sync_vault_salt = "salt-a".into();
        settings.sync_base_revision = 5;
        let snap2 = make_snap("epoch-1", "salt-a", 6);
        assert_eq!(detect_sync_conflict(&settings, Some(&snap1.vault), &snap2), None);

        // 3. 另一台设备选择“以本地强制覆盖云端”生成新 epoch（即使 revision 较小）→ 触发 RemoteDiverged
        let snap_reset = make_snap("epoch-2", "salt-a", 2);
        assert_eq!(
            detect_sync_conflict(&settings, Some(&snap1.vault), &snap_reset),
            Some(SyncConflictReason::RemoteDiverged)
        );

        // 4. 另一台设备更换了密钥库（salt 变化）→ 触发 RemoteDiverged
        let snap_rekeyed = make_snap("epoch-1", "salt-b", 6);
        assert_eq!(
            detect_sync_conflict(&settings, Some(&snap1.vault), &snap_rekeyed),
            Some(SyncConflictReason::RemoteDiverged)
        );
    }
}
