//! HoloSSH 跨平台端到端加密密钥库服务（Argon2id + AES-256-GCM）
//!
//! 设计原则：
//! - 完全跨平台（macOS / Linux / Windows），不依赖系统钥匙串；
//! - 密钥库以主密码经 Argon2id 派生 256-bit 密钥，并用 AES-256-GCM 认证加密保存至 `vault.json`；
//! - 主机密码、导入的私钥 PEM 原文、云端同步 Token/Secret Key 均关联主机 ID 加密存放于密钥库中，
//!   云端同步时直接将已加密的 `vault.json` 密文与主机列表一起同步，云端永不见明文；
//! - 支持「不记住密码 / 记住 1/3/5/7/15/30 天」策略，默认「不记住密码」。

use crate::i18n::{tr, trf};
use crate::state::host::now_unix_ms;
use crate::state::settings::{AppSettings, VaultRememberPolicy};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use chrono::Local;
use curve25519_dalek::montgomery::MontgomeryPoint;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroize;

const VAULT_VERSION: u32 = 1;
const ARGON2_M_COST: u32 = 19_456; // 19 MiB（OWASP 推荐参数，兼顾抗暴力破解与桌面端流畅解锁）
const ARGON2_T_COST: u32 = 2;
const ARGON2_P_COST: u32 = 1;

/// 与单台主机强关联的加密凭据记录
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VaultSecretEntry {
    pub host_id: String,
    pub host_name: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub password: Option<String>,
    /// 私钥 PEM 文本内容（跨设备同步后无需本机文件即可直接认证）
    #[serde(default)]
    pub private_key_pem: Option<String>,
    #[serde(default)]
    pub private_key_name: Option<String>,
    #[serde(default)]
    pub passphrase: Option<String>,
    #[serde(default)]
    pub updated_at: String,
    /// UTC 毫秒时间戳（用于跨设备 LWW 合并）
    #[serde(default)]
    pub updated_at_ms: u64,
}

impl VaultSecretEntry {
    pub fn has_any_secret(&self) -> bool {
        self.password.as_deref().is_some_and(|s| !s.is_empty())
            || self
                .private_key_pem
                .as_deref()
                .is_some_and(|s| !s.is_empty())
            || self.passphrase.as_deref().is_some_and(|s| !s.is_empty())
    }
}

/// 密钥库解密后的明文业务结构（仅存在于内存，绝不以明文落盘）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VaultData {
    #[serde(default)]
    pub host_secrets: HashMap<String, VaultSecretEntry>,
    /// X25519 拆封私钥（仅存放于主密码加密的密文内，绝不明文落盘）
    #[serde(default)]
    pub write_private_key_hex: String,
}

/// 通用 X25519 + AES-256-GCM 公钥封装密文（用于锁定态凭据写入及全量加密同步目录/增量补丁）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealedBlob {
    pub eph_pubkey_hex: String,
    pub nonce_hex: String,
    pub ciphertext_hex: String,
}

/// 磁盘与云端同步共用的加密信封格式（Argon2id + AES-256-GCM + X25519 写入公钥）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedVaultFile {
    pub version: u32,
    pub kdf: String,
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub salt_hex: String,
    pub nonce_hex: String,
    pub ciphertext_hex: String,
    /// X25519 写入公钥：锁定态允许安全加密写入新凭据与同步快照，但无法解密任何已有内容
    #[serde(default)]
    pub write_public_key_hex: String,
    /// 锁定态写入待拆封合并的密文条目（下次主密码解锁时自动拆封合并入 `host_secrets`）
    #[serde(default)]
    pub sealed_entries: HashMap<String, SealedBlob>,
    /// 锁定态删除的主机 ID 标记（下次主密码解锁时同步清理）
    #[serde(default)]
    pub pending_deletes: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RememberedSessionFile {
    expires_at_unix: u64,
    nonce_hex: String,
    wrapped_key_hex: String,
}

pub fn vault_path() -> Option<PathBuf> {
    AppSettings::config_dir().map(|d| d.join("vault.json"))
}

fn session_path() -> Option<PathBuf> {
    AppSettings::config_dir().map(|d| d.join("vault_session.json"))
}

pub fn vault_exists() -> bool {
    vault_path().map(|p| p.is_file()).unwrap_or(false)
}

/// 当前密钥库是否具备 X25519 写入公钥（允许锁定态免解锁直接加密写入）
pub fn vault_supports_sealed_write() -> bool {
    load_encrypted_file()
        .and_then(|enc| decode_32_hex(&enc.write_public_key_hex))
        .is_some()
}

pub fn load_encrypted_file() -> Option<EncryptedVaultFile> {
    let path = vault_path()?;
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn save_encrypted_file(file: &EncryptedVaultFile) -> Result<(), String> {
    let path = vault_path().ok_or_else(|| tr("svc.vault.no_dir").to_string())?;
    write_json_secure(&path, file)
}

/// 首次初始化密钥库：校验主密码、生成 X25519 写入密钥对并写入全新加密 `vault.json`
pub fn create_new_vault(master_password: &str) -> Result<([u8; 32], VaultData), String> {
    if master_password.len() < 4 {
        return Err(tr("toast.vault_pwd_too_short").into());
    }
    let mut salt = [0u8; 16];
    fill_random(&mut salt)?;
    let key = derive_key(
        master_password,
        &salt,
        ARGON2_M_COST,
        ARGON2_T_COST,
        ARGON2_P_COST,
    )?;
    let (sk, pk) = generate_x25519_keypair()?;
    let data = VaultData {
        write_private_key_hex: hex::encode(sk),
        ..VaultData::default()
    };
    let enc = encrypt_vault_data(
        &key,
        &salt,
        ARGON2_M_COST,
        ARGON2_T_COST,
        ARGON2_P_COST,
        &data,
        hex::encode(pk),
    )?;
    save_encrypted_file(&enc)?;
    Ok((key, data))
}

/// 使用主密码解锁现有密钥库，自动拆封合并锁定期间写入的密文条目，返回派生密钥与解密数据
pub fn unlock_vault(master_password: &str) -> Result<([u8; 32], VaultData), String> {
    let enc = load_encrypted_file().ok_or_else(|| tr("svc.vault.not_created").to_string())?;
    let (key, mut data) = unlock_encrypted_file(&enc, master_password)?;
    let needs_consolidate = !enc.sealed_entries.is_empty()
        || !enc.pending_deletes.is_empty()
        || decode_32_hex(&enc.write_public_key_hex).is_none()
        || decode_32_hex(&data.write_private_key_hex).is_none();
    if needs_consolidate {
        ensure_write_keypair(&mut data)?;
        let _ = save_vault_with_key(&key, &data);
    }
    Ok((key, data))
}

pub fn unlock_encrypted_file(
    enc: &EncryptedVaultFile,
    master_password: &str,
) -> Result<([u8; 32], VaultData), String> {
    let salt = hex::decode(&enc.salt_hex).map_err(|_| tr("svc.vault.corrupt_salt").to_string())?;
    let key = derive_key(master_password, &salt, enc.m_cost, enc.t_cost, enc.p_cost)?;
    let data = decrypt_vault_data(&key, enc)?;
    Ok((key, data))
}

/// 使用已派生（或记住期限内）的 32 字节密钥解密当前密钥库，并自动合并锁定期间写入的封装条目
pub fn unlock_vault_with_key(key: &[u8; 32]) -> Result<VaultData, String> {
    let enc = load_encrypted_file().ok_or_else(|| tr("svc.vault.not_created").to_string())?;
    let mut data = decrypt_vault_data(key, &enc)?;
    let needs_consolidate = !enc.sealed_entries.is_empty()
        || !enc.pending_deletes.is_empty()
        || decode_32_hex(&enc.write_public_key_hex).is_none()
        || decode_32_hex(&data.write_private_key_hex).is_none();
    if needs_consolidate {
        ensure_write_keypair(&mut data)?;
        let _ = save_vault_with_key(key, &data);
    }
    Ok(data)
}

/// 用当前已解锁密钥将更新后的 `VaultData` 重新加密（每次生成全新随机 Nonce）并落盘
pub fn save_vault_with_key(key: &[u8; 32], data: &VaultData) -> Result<EncryptedVaultFile, String> {
    let existing = load_encrypted_file().ok_or_else(|| tr("svc.vault.file_missing").to_string())?;
    let salt =
        hex::decode(&existing.salt_hex).map_err(|_| tr("svc.vault.corrupt_salt").to_string())?;
    let mut data_owned = data.clone();
    let write_pub_hex = ensure_write_keypair(&mut data_owned)?;
    let enc = encrypt_vault_data(
        key,
        &salt,
        existing.m_cost,
        existing.t_cost,
        existing.p_cost,
        &data_owned,
        write_pub_hex,
    )?;
    save_encrypted_file(&enc)?;
    Ok(enc)
}

/// 修改密钥库主密码：验证原密码正确后，使用全新随机盐与 Nonce 重新加密全部凭据
pub fn change_master_password(
    old_password: &str,
    new_password: &str,
) -> Result<([u8; 32], VaultData), String> {
    if new_password.len() < 4 {
        return Err(tr("toast.vault_new_pwd_short").into());
    }
    let (mut old_key, mut data) =
        unlock_vault(old_password).map_err(|_| tr("svc.vault.old_pwd_wrong").to_string())?;
    old_key.zeroize();

    let mut new_salt = [0u8; 16];
    fill_random(&mut new_salt)?;
    let new_key = derive_key(
        new_password,
        &new_salt,
        ARGON2_M_COST,
        ARGON2_T_COST,
        ARGON2_P_COST,
    )?;
    let write_pub_hex = ensure_write_keypair(&mut data)?;
    let enc = encrypt_vault_data(
        &new_key,
        &new_salt,
        ARGON2_M_COST,
        ARGON2_T_COST,
        ARGON2_P_COST,
        &data,
        write_pub_hex,
    )?;
    save_encrypted_file(&enc)?;
    Ok((new_key, data))
}

/// 锁定态免解锁直接加密写入单台主机凭据（使用 `vault.json` 中的 X25519 公钥进行 Sealed Box 封装）
///
/// 返回读取到的私钥 PEM（供新建后立即连接使用）；若未填写任何密码/私钥则返回 `Ok(None)`。
pub fn seal_host_secret_to_locked_vault(
    host_id: &str,
    host_name: &str,
    password: Option<String>,
    private_key_path: Option<&str>,
) -> Result<Option<String>, String> {
    let entry = build_secret_entry(host_id, host_name, password, private_key_path, None);
    let key_pem = entry.private_key_pem.clone();
    if !entry.has_any_secret() {
        return Ok(None);
    }

    let mut enc = load_encrypted_file().ok_or_else(|| tr("svc.vault.not_created").to_string())?;
    let pub_bytes = decode_32_hex(&enc.write_public_key_hex)
        .ok_or_else(|| tr("svc.vault.encrypt_err").to_string())?;
    let sealed = seal_value_with_pubkey(&pub_bytes, &entry)?;
    enc.sealed_entries.insert(host_id.to_string(), sealed);
    enc.pending_deletes.retain(|id| id != host_id);
    save_encrypted_file(&enc)?;
    Ok(key_pem)
}

/// 锁定态删除主机时同步标记待删除项并清理未拆封条目
pub fn mark_deleted_in_locked_vault(host_id: &str) {
    let Some(mut enc) = load_encrypted_file() else {
        return;
    };
    enc.sealed_entries.remove(host_id);
    if !enc.pending_deletes.iter().any(|id| id == host_id) {
        enc.pending_deletes.push(host_id.to_string());
    }
    let _ = save_encrypted_file(&enc);
}

fn build_secret_entry(
    host_id: &str,
    host_name: &str,
    password: Option<String>,
    private_key_path: Option<&str>,
    prev_entry: Option<&VaultSecretEntry>,
) -> VaultSecretEntry {
    let clean_pwd = password.filter(|p| !p.is_empty());
    let mut key_pem: Option<String> = None;
    let mut key_name: Option<String> = None;

    if let Some(raw_path) = private_key_path.map(str::trim).filter(|s| !s.is_empty()) {
        let expanded = crate::services::ssh_keys::expand_tilde(raw_path);
        if let Ok(pem_text) = fs::read_to_string(&expanded) {
            if !pem_text.trim().is_empty() && pem_text.len() <= 64 * 1024 {
                key_pem = Some(pem_text);
                key_name = expanded
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string());
            }
        } else if let Some(prev) = prev_entry {
            key_pem = prev.private_key_pem.clone();
            key_name = prev.private_key_name.clone();
        }
    }

    VaultSecretEntry {
        host_id: host_id.to_string(),
        host_name: host_name.to_string(),
        hostname: prev_entry.map(|p| p.hostname.clone()).unwrap_or_default(),
        password: clean_pwd,
        private_key_pem: key_pem,
        private_key_name: key_name,
        passphrase: None,
        updated_at: Local::now().format("%Y-%m-%d %H:%M").to_string(),
        updated_at_ms: now_unix_ms(),
    }
}

/// 将主机密码与私钥文件（自动读取 PEM 原文以便跨设备同步）关联写入密钥库内存对象
pub fn upsert_host_secret(
    data: &mut VaultData,
    host_id: &str,
    host_name: &str,
    password: Option<String>,
    private_key_path: Option<&str>,
) {
    let prev = data.host_secrets.get(host_id).cloned();
    let entry = build_secret_entry(host_id, host_name, password, private_key_path, prev.as_ref());
    if entry.has_any_secret() {
        data.host_secrets.insert(host_id.to_string(), entry);
    } else {
        data.host_secrets.remove(host_id);
    }
}

/// 按「记住密码策略」持久化或清除限时免密会话
pub fn apply_remember_session(key: &[u8; 32], policy: VaultRememberPolicy) {
    let Some(secs) = policy.duration_secs() else {
        clear_remember_session();
        return;
    };
    let Some(path) = session_path() else {
        return;
    };
    let expires_at_unix = now_unix().saturating_add(secs);
    let mut wrap_key = derive_device_wrap_key();
    let mut nonce_bytes = [0u8; 12];
    if fill_random(&mut nonce_bytes).is_err() {
        wrap_key.zeroize();
        return;
    }
    if let Ok(cipher) = Aes256Gcm::new_from_slice(&wrap_key) {
        let nonce = Nonce::from(nonce_bytes);
        if let Ok(wrapped) = cipher.encrypt(&nonce, key.as_ref()) {
            let record = RememberedSessionFile {
                expires_at_unix,
                nonce_hex: hex::encode(nonce_bytes),
                wrapped_key_hex: hex::encode(wrapped),
            };
            let _ = write_json_secure(&path, &record);
        }
    }
    wrap_key.zeroize();
}

/// 若存在未过期的记住会话，自动恢复 32 字节密钥；已过期则自动删除会话文件
pub fn try_load_remembered_key(policy: VaultRememberPolicy) -> Option<[u8; 32]> {
    if policy == VaultRememberPolicy::Never {
        clear_remember_session();
        return None;
    }
    let path = session_path()?;
    let raw = fs::read_to_string(&path).ok()?;
    let record: RememberedSessionFile = serde_json::from_str(&raw).ok()?;
    if now_unix() >= record.expires_at_unix {
        clear_remember_session();
        return None;
    }
    let nonce_bytes = hex::decode(&record.nonce_hex).ok()?;
    let wrapped = hex::decode(&record.wrapped_key_hex).ok()?;
    let nonce_arr: [u8; 12] = nonce_bytes.try_into().ok()?;
    let mut wrap_key = derive_device_wrap_key();
    let cipher = Aes256Gcm::new_from_slice(&wrap_key).ok();
    wrap_key.zeroize();
    let nonce = Nonce::from(nonce_arr);
    let plain = cipher?.decrypt(&nonce, wrapped.as_ref()).ok()?;
    if plain.len() != 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&plain);
    Some(out)
}

pub fn clear_remember_session() {
    if let Some(path) = session_path() {
        let _ = fs::remove_file(path);
    }
}

/// 使用已派生密钥解密任意加密信封（供全量加密同步引擎调用）
pub fn decrypt_vault_with_key(
    key: &[u8; 32],
    enc: &EncryptedVaultFile,
) -> Result<VaultData, String> {
    decrypt_vault_data(key, enc)
}

/// 沿用指定加密信封的盐与 Argon2 参数重新加密 `VaultData`（不写盘，供同步引擎构建压缩快照）
pub fn encrypt_vault_with_template(
    key: &[u8; 32],
    template: &EncryptedVaultFile,
    data: &VaultData,
) -> Result<EncryptedVaultFile, String> {
    let salt =
        hex::decode(&template.salt_hex).map_err(|_| tr("svc.vault.corrupt_salt").to_string())?;
    let mut data_owned = data.clone();
    let write_pub_hex = ensure_write_keypair(&mut data_owned)?;
    encrypt_vault_data(
        key,
        &salt,
        template.m_cost,
        template.t_cost,
        template.p_cost,
        &data_owned,
        write_pub_hex,
    )
}

/// 使用 X25519 公钥（hex）将任意可序列化对象封装为 `SealedBlob`
pub fn seal_json_with_pubkey<T: Serialize>(
    pubkey_hex: &str,
    value: &T,
) -> Result<SealedBlob, String> {
    let pub_bytes =
        decode_32_hex(pubkey_hex).ok_or_else(|| tr("svc.vault.encrypt_err").to_string())?;
    seal_value_with_pubkey(&pub_bytes, value)
}

/// 使用已解锁 `VaultData` 内的 X25519 私钥拆封 `SealedBlob`
pub fn unseal_json_with_vault<T: for<'de> Deserialize<'de>>(
    data: &VaultData,
    blob: &SealedBlob,
) -> Result<T, String> {
    let mut priv_bytes = decode_32_hex(&data.write_private_key_hex)
        .ok_or_else(|| tr("svc.vault.wrong_pwd").to_string())?;
    let pub_bytes = MontgomeryPoint::mul_base_clamped(priv_bytes).to_bytes();
    let res = unseal_value_with_privkey(&priv_bytes, &pub_bytes, blob);
    priv_bytes.zeroize();
    res
}

/// 判断两份加密信封是否属于同一个密钥库写入公钥
pub fn has_same_write_pubkey(a: &EncryptedVaultFile, b: &EncryptedVaultFile) -> bool {
    decode_32_hex(&a.write_public_key_hex).is_some()
        && a.write_public_key_hex.eq_ignore_ascii_case(&b.write_public_key_hex)
}

/// 按 `updated_at_ms` + 删除墓碑合并本地与远端主机凭据（LWW-Element-Set）
pub fn merge_vault_secrets(
    local: &mut VaultData,
    remote: &VaultData,
    deleted_hosts: &BTreeMap<String, u64>,
) {
    for (hid, r_entry) in &remote.host_secrets {
        let should_take = match local.host_secrets.get(hid) {
            None => true,
            Some(l_entry) => r_entry.updated_at_ms >= l_entry.updated_at_ms,
        };
        if should_take {
            local.host_secrets.insert(hid.clone(), r_entry.clone());
        }
    }
    local.host_secrets.retain(|hid, entry| {
        deleted_hosts
            .get(hid)
            .is_none_or(|&del_ms| del_ms < entry.updated_at_ms)
    });
}

fn derive_key(
    password: &str,
    salt: &[u8],
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
) -> Result<[u8; 32], String> {
    let params = Params::new(m_cost, t_cost, p_cost, Some(32))
        .map_err(|e| trf("svc.vault.kdf_param_err", &[("err", &e.to_string())]))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| trf("svc.vault.kdf_err", &[("err", &e.to_string())]))?;
    Ok(key)
}

fn encrypt_vault_data(
    key: &[u8; 32],
    salt: &[u8],
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
    data: &VaultData,
    write_public_key_hex: String,
) -> Result<EncryptedVaultFile, String> {
    let plaintext = serde_json::to_vec(data)
        .map_err(|e| trf("svc.vault.serialize_err", &[("err", &e.to_string())]))?;
    let mut nonce_bytes = [0u8; 12];
    fill_random(&mut nonce_bytes)?;
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| trf("svc.vault.cipher_init_err", &[("err", &e.to_string())]))?;
    let nonce = Nonce::from(nonce_bytes);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_ref())
        .map_err(|_| tr("svc.vault.encrypt_err").to_string())?;
    Ok(EncryptedVaultFile {
        version: VAULT_VERSION,
        kdf: "argon2id".into(),
        m_cost,
        t_cost,
        p_cost,
        salt_hex: hex::encode(salt),
        nonce_hex: hex::encode(nonce_bytes),
        ciphertext_hex: hex::encode(ciphertext),
        write_public_key_hex,
        sealed_entries: HashMap::new(),
        pending_deletes: Vec::new(),
    })
}

fn decrypt_vault_data(key: &[u8; 32], enc: &EncryptedVaultFile) -> Result<VaultData, String> {
    let nonce_bytes =
        hex::decode(&enc.nonce_hex).map_err(|_| tr("svc.vault.corrupt_nonce").to_string())?;
    let ciphertext = hex::decode(&enc.ciphertext_hex)
        .map_err(|_| tr("svc.vault.corrupt_ciphertext").to_string())?;
    let nonce_arr: [u8; 12] = nonce_bytes
        .try_into()
        .map_err(|_| tr("svc.vault.invalid_nonce_len").to_string())?;
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| tr("svc.vault.decrypt_init_err").to_string())?;
    let nonce = Nonce::from(nonce_arr);
    let plaintext = cipher
        .decrypt(&nonce, ciphertext.as_ref())
        .map_err(|_| tr("svc.vault.wrong_pwd").to_string())?;
    let mut data: VaultData = serde_json::from_slice(&plaintext)
        .map_err(|e| trf("svc.vault.parse_err", &[("err", &e.to_string())]))?;

    // 应用锁定期间的待删除标记，并用 X25519 私钥自动拆封合并锁定期间写入的密文条目
    for del_id in &enc.pending_deletes {
        data.host_secrets.remove(del_id);
    }
    if !enc.sealed_entries.is_empty() {
        if let Some(mut priv_bytes) = decode_32_hex(&data.write_private_key_hex) {
            let pub_bytes = MontgomeryPoint::mul_base_clamped(priv_bytes).to_bytes();
            for (hid, sealed) in &enc.sealed_entries {
                if let Ok(entry) =
                    unseal_value_with_privkey::<VaultSecretEntry>(&priv_bytes, &pub_bytes, sealed)
                {
                    if entry.has_any_secret() {
                        data.host_secrets.insert(hid.clone(), entry);
                    }
                }
            }
            priv_bytes.zeroize();
        }
    }
    Ok(data)
}

fn generate_x25519_keypair() -> Result<([u8; 32], [u8; 32]), String> {
    let mut sk = [0u8; 32];
    fill_random(&mut sk)?;
    let pk = MontgomeryPoint::mul_base_clamped(sk).to_bytes();
    Ok((sk, pk))
}

fn ensure_write_keypair(data: &mut VaultData) -> Result<String, String> {
    if let Some(sk) = decode_32_hex(&data.write_private_key_hex) {
        let pk = MontgomeryPoint::mul_base_clamped(sk).to_bytes();
        return Ok(hex::encode(pk));
    }
    let (sk, pk) = generate_x25519_keypair()?;
    data.write_private_key_hex = hex::encode(sk);
    Ok(hex::encode(pk))
}

fn decode_32_hex(s: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(s.trim()).ok()?;
    bytes.try_into().ok()
}

fn derive_seal_aead_key(
    eph_pub: &[u8; 32],
    recipient_pub: &[u8; 32],
    shared_secret: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"holossh-vault-seal-v1|");
    hasher.update(eph_pub);
    hasher.update(recipient_pub);
    hasher.update(shared_secret);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn seal_value_with_pubkey<T: Serialize>(
    recipient_pub: &[u8; 32],
    value: &T,
) -> Result<SealedBlob, String> {
    let plaintext = serde_json::to_vec(value)
        .map_err(|e| trf("svc.vault.serialize_err", &[("err", &e.to_string())]))?;
    let (mut eph_sk, eph_pk) = generate_x25519_keypair()?;
    let mut shared = MontgomeryPoint(*recipient_pub)
        .mul_clamped(eph_sk)
        .to_bytes();
    eph_sk.zeroize();
    let mut seal_key = derive_seal_aead_key(&eph_pk, recipient_pub, &shared);
    shared.zeroize();

    let mut nonce_bytes = [0u8; 12];
    fill_random(&mut nonce_bytes)?;
    let cipher = Aes256Gcm::new_from_slice(&seal_key)
        .map_err(|e| trf("svc.vault.cipher_init_err", &[("err", &e.to_string())]))?;
    seal_key.zeroize();
    let nonce = Nonce::from(nonce_bytes);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_ref())
        .map_err(|_| tr("svc.vault.encrypt_err").to_string())?;
    Ok(SealedBlob {
        eph_pubkey_hex: hex::encode(eph_pk),
        nonce_hex: hex::encode(nonce_bytes),
        ciphertext_hex: hex::encode(ciphertext),
    })
}

fn unseal_value_with_privkey<T: for<'de> Deserialize<'de>>(
    recipient_priv: &[u8; 32],
    recipient_pub: &[u8; 32],
    sealed: &SealedBlob,
) -> Result<T, String> {
    let eph_pub = decode_32_hex(&sealed.eph_pubkey_hex)
        .ok_or_else(|| tr("svc.vault.corrupt_ciphertext").to_string())?;
    let nonce_bytes =
        hex::decode(&sealed.nonce_hex).map_err(|_| tr("svc.vault.corrupt_nonce").to_string())?;
    let nonce_arr: [u8; 12] = nonce_bytes
        .try_into()
        .map_err(|_| tr("svc.vault.invalid_nonce_len").to_string())?;
    let ciphertext = hex::decode(&sealed.ciphertext_hex)
        .map_err(|_| tr("svc.vault.corrupt_ciphertext").to_string())?;

    let mut shared = MontgomeryPoint(eph_pub)
        .mul_clamped(*recipient_priv)
        .to_bytes();
    let mut seal_key = derive_seal_aead_key(&eph_pub, recipient_pub, &shared);
    shared.zeroize();

    let cipher = Aes256Gcm::new_from_slice(&seal_key)
        .map_err(|_| tr("svc.vault.decrypt_init_err").to_string())?;
    seal_key.zeroize();
    let nonce = Nonce::from(nonce_arr);
    let plaintext = cipher
        .decrypt(&nonce, ciphertext.as_ref())
        .map_err(|_| tr("svc.vault.wrong_pwd").to_string())?;
    serde_json::from_slice(&plaintext)
        .map_err(|e| trf("svc.vault.parse_err", &[("err", &e.to_string())]))
}

fn derive_device_wrap_key() -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"holossh-vault-session-v1|");
    if let Some(dir) = AppSettings::config_dir() {
        hasher.update(dir.to_string_lossy().as_bytes());
    }
    if let Ok(user) = std::env::var("USER").or_else(|_| std::env::var("USERNAME")) {
        hasher.update(b"|");
        hasher.update(user.as_bytes());
    }
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn fill_random(buf: &mut [u8]) -> Result<(), String> {
    getrandom::fill(buf).map_err(|e| trf("svc.vault.rng_err", &[("err", &e.to_string())]))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn write_json_secure<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| trf("svc.vault.mkdir_err", &[("err", &e.to_string())]))?;
    }
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| trf("svc.vault.serialize_err", &[("err", &e.to_string())]))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)
        .map_err(|e| trf("svc.vault.write_tmp_err", &[("err", &e.to_string())]))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, path).map_err(|e| trf("svc.vault.save_err", &[("err", &e.to_string())]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon2_aes_gcm_roundtrip_and_password_verification() {
        let mut salt = [0u8; 16];
        fill_random(&mut salt).unwrap();
        let key = derive_key("master-secret-123", &salt, 8192, 1, 1).unwrap();

        let (sk, pk) = generate_x25519_keypair().unwrap();
        let mut data = VaultData {
            write_private_key_hex: hex::encode(sk),
            ..VaultData::default()
        };
        data.host_secrets.insert(
            "host-1".into(),
            VaultSecretEntry {
                host_id: "host-1".into(),
                host_name: "prod-web".into(),
                hostname: "10.0.0.1".into(),
                password: Some("s3cr3t!".into()),
                private_key_pem: Some("-----BEGIN OPENSSH PRIVATE KEY-----\nTEST\n".into()),
                private_key_name: Some("id_ed25519".into()),
                passphrase: None,
                updated_at: "2026-11-22 11:45".into(),
                updated_at_ms: 1000,
            },
        );

        let mut enc = encrypt_vault_data(&key, &salt, 8192, 1, 1, &data, hex::encode(pk)).unwrap();
        assert!(!enc.ciphertext_hex.contains("s3cr3t"));

        // 模拟锁定态免解锁使用公钥封装写入第二台主机凭据
        let sealed_entry = VaultSecretEntry {
            host_id: "host-2".into(),
            host_name: "edge-node".into(),
            hostname: "10.0.0.2".into(),
            password: Some("locked-write-pwd!".into()),
            private_key_pem: None,
            private_key_name: None,
            passphrase: None,
            updated_at: "2026-11-22 15:55".into(),
            updated_at_ms: 2000,
        };
        let sealed = seal_value_with_pubkey(&pk, &sealed_entry).unwrap();
        assert!(!sealed.ciphertext_hex.contains("locked-write-pwd"));
        enc.sealed_entries.insert("host-2".into(), sealed);

        // 解锁时自动拆封合并锁定期间写入的密文
        let (_, decoded) = unlock_encrypted_file(&enc, "master-secret-123").unwrap();
        assert_eq!(
            decoded.host_secrets["host-1"].password.as_deref(),
            Some("s3cr3t!")
        );
        assert_eq!(
            decoded.host_secrets["host-2"].password.as_deref(),
            Some("locked-write-pwd!")
        );

        let wrong = unlock_encrypted_file(&enc, "wrong-password");
        assert!(wrong.is_err());
    }
}
