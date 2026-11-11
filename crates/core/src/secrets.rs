//! 密钥存储：**本地加密文件**，不进系统钥匙串。
//!
//! 布局（都在 `~/Library/Application Support/Vox/` 下）：
//!
//! ```text
//! master.key    32 字节随机主密钥，权限 0600
//! secrets.json  { "version": 1, "entries": { "<account>": {nonce, ciphertext} } }
//! ```
//!
//! 加密用 XChaCha20-Poly1305（AEAD），每个条目一个随机 24 字节 nonce，
//! 并把 account 名作为 AAD —— 密文块换到别的条目下解不开。
//!
//! ## 威胁模型（别高估它的能力）
//!
//! - 能防：配置目录被复制/备份外泄、截图、把文件误发给别人、同一台机器上的
//!   另一个用户账号读取你的家目录
//! - 防不了：已经能以你的身份运行的恶意程序（它同样能读到 `master.key`）
//!
//! 要更强的保护需要口令派生（Argon2），但那等于给 Vox 加了「登录」，
//! 与「无登录」的产品定位冲突，所以这里不做。
//!
//! ## 兼容
//!
//! - 老配置里的 `api_key_ref = "keyring:vox.stt"` 继续可用：前缀只是名字，
//!   实际按 account 名在**文件**里找
//! - 老的钥匙串条目仍可读（迁移来源），但**写入只进文件**

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};

/// 加密后的密钥文件名。
const SECRETS_FILE: &str = "secrets.json";
/// 主密钥文件名。
const MASTER_KEY_FILE: &str = "master.key";
/// 主密钥长度。
const KEY_LEN: usize = 32;
/// XChaCha20-Poly1305 的 nonce 长度。
const NONCE_LEN: usize = 24;
/// 老钥匙串条目的 service 名（只用于迁移）。
const LEGACY_SERVICE: &str = "vox";

// ---------------------------------------------------------------------------
// 文件格式
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct SecretsFile {
    version: u32,
    #[serde(default)]
    entries: BTreeMap<String, Sealed>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Sealed {
    /// hex 编码的 nonce
    nonce: String,
    /// hex 编码的密文（含 Poly1305 tag）
    ciphertext: String,
}

// ---------------------------------------------------------------------------
// 路径
// ---------------------------------------------------------------------------

fn secrets_path() -> PathBuf {
    Config::app_dir().join(SECRETS_FILE)
}

fn master_key_path() -> PathBuf {
    Config::app_dir().join(MASTER_KEY_FILE)
}

/// 以 0600 权限写文件（创建时即限定，避免出现短暂的 0644 窗口）。
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    let mut f = {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(path)?
    };
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 主密钥
// ---------------------------------------------------------------------------

fn load_master_key() -> Result<[u8; KEY_LEN]> {
    let path = master_key_path();

    if path.exists() {
        let bytes = fs::read(&path)?;
        if bytes.len() != KEY_LEN {
            return Err(Error::Config(format!(
                "主密钥 {} 长度异常（{} 字节，应为 {KEY_LEN}）",
                path.display(),
                bytes.len()
            )));
        }
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }

    // 首次使用：生成随机主密钥
    let mut key = [0u8; KEY_LEN];
    getrandom::getrandom(&mut key).map_err(|e| Error::Config(format!("生成主密钥失败: {e}")))?;
    fs::create_dir_all(Config::app_dir())?;
    write_private(&path, &key)?;
    Ok(key)
}

// ---------------------------------------------------------------------------
// 加解密
// ---------------------------------------------------------------------------

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

fn seal(key: &[u8; KEY_LEN], account: &str, plaintext: &str) -> Result<Sealed> {
    let cipher =
        XChaCha20Poly1305::new_from_slice(key).map_err(|e| Error::Config(format!("初始化加密器失败: {e}")))?;
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce).map_err(|e| Error::Config(format!("生成 nonce 失败: {e}")))?;
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext.as_bytes(),
                aad: account.as_bytes(),
            },
        )
        .map_err(|e| Error::Config(format!("加密失败: {e}")))?;
    Ok(Sealed {
        nonce: hex_encode(&nonce),
        ciphertext: hex_encode(&ciphertext),
    })
}

fn unseal(key: &[u8; KEY_LEN], account: &str, sealed: &Sealed) -> Result<String> {
    let nonce =
        hex_decode(&sealed.nonce).ok_or_else(|| Error::Config(format!("密钥 `{account}` 的 nonce 格式错误")))?;
    let ciphertext = hex_decode(&sealed.ciphertext)
        .ok_or_else(|| Error::Config(format!("密钥 `{account}` 的密文格式错误")))?;
    if nonce.len() != NONCE_LEN {
        return Err(Error::Config(format!("密钥 `{account}` 的 nonce 长度错误")));
    }
    let cipher =
        XChaCha20Poly1305::new_from_slice(key).map_err(|e| Error::Config(format!("初始化解密器失败: {e}")))?;
    let plain = cipher
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: account.as_bytes(),
            },
        )
        .map_err(|_| {
            Error::Config(format!(
                "密钥 `{account}` 解密失败（文件被改动或主密钥不匹配）"
            ))
        })?;
    String::from_utf8(plain).map_err(|e| Error::Config(format!("密钥 `{account}` 不是合法 UTF-8: {e}")))
}

// ---------------------------------------------------------------------------
// 密钥文件读写
// ---------------------------------------------------------------------------

fn load_file() -> Result<SecretsFile> {
    let path = secrets_path();
    if !path.exists() {
        return Ok(SecretsFile {
            version: 1,
            entries: BTreeMap::new(),
        });
    }
    let text = fs::read_to_string(&path)?;
    let file: SecretsFile = serde_json::from_str(&text)
        .map_err(|e| Error::Config(format!("解析 {} 失败: {e}", path.display())))?;
    if file.version != 1 {
        return Err(Error::Config(format!(
            "{} 的版本 {} 不受支持",
            path.display(),
            file.version
        )));
    }
    Ok(file)
}

fn save_file(file: &SecretsFile) -> Result<()> {
    fs::create_dir_all(Config::app_dir())?;
    let text =
        serde_json::to_string_pretty(file).map_err(|e| Error::Config(format!("序列化密钥文件失败: {e}")))?;
    write_private(&secrets_path(), text.as_bytes())
}

/// 从加密文件读一个密钥。
fn read_sealed(account: &str) -> Option<String> {
    let file = load_file().ok()?;
    let sealed = file.entries.get(account)?;
    let key = load_master_key().ok()?;
    let plain = unseal(&key, account, sealed).ok()?;
    if plain.trim().is_empty() {
        None
    } else {
        Some(plain)
    }
}

// ---------------------------------------------------------------------------
// 对外 API
// ---------------------------------------------------------------------------

/// 去掉 `keyring:` / `file:` / `secret:` 这类前缀，只留 account 名。
fn account_of(key_ref: &str) -> &str {
    for prefix in ["keyring:", "file:", "secret:", "vox:"] {
        if let Some(rest) = key_ref.strip_prefix(prefix) {
            return rest;
        }
    }
    key_ref
}

/// 解析 API key。
///
/// 优先级：`api_key`（内联明文，仅调试用）> 本地加密文件 > 老钥匙串（迁移来源）。
pub fn resolve(inline: Option<&str>, key_ref: Option<&str>) -> Result<String> {
    if let Some(k) = inline {
        let k = k.trim();
        if !k.is_empty() {
            return Ok(k.to_string());
        }
    }
    if let Some(r) = key_ref {
        let r = r.trim();
        if r.is_empty() {
            return Err(Error::Config("api_key 与 api_key_ref 均为空".into()));
        }
        let account = account_of(r);

        // ① 本地加密文件（正式来源）
        if let Some(v) = read_sealed(account) {
            return Ok(v);
        }

        // ② 老钥匙串（迁移来源）
        if let Some(v) = read_keychain(account) {
            return Ok(v);
        }

        return Err(Error::Config(format!(
            "找不到密钥 `{account}`。在 Vox 设置的「模型」页填好并保存即可（会加密存到 {}）",
            secrets_path().display()
        )));
    }
    Err(Error::Config("未配置 API key（需要 api_key 或 api_key_ref）".into()))
}

/// 写入密钥：**只写加密文件**。
///
/// 注意：这里**不会**去动系统钥匙串。清理老钥匙串副本是
/// [`migrate_from_keychain`] 的职责（它会先验证文件里确实能读出来再删）。
pub fn store(account: &str, secret: &str) -> Result<()> {
    let key = load_master_key()?;
    let sealed = seal(&key, account, secret)?;

    let mut file = load_file()?;
    file.entries.insert(account.to_string(), sealed);
    save_file(&file)?;

    Ok(())
}

/// 删除一个密钥。返回是否确实删掉了。
pub fn delete(account: &str) -> Result<bool> {
    let mut file = load_file()?;
    let removed = file.entries.remove(account).is_some();
    if removed {
        save_file(&file)?;
    }
    Ok(removed)
}

/// 列出已存的 account 名。
pub fn list() -> Result<Vec<String>> {
    Ok(load_file()?.entries.keys().cloned().collect())
}

/// 某个 account 是否已配置。
pub fn has(account: &str) -> bool {
    load_file()
        .map(|f| f.entries.contains_key(account))
        .unwrap_or(false)
}

/// 把老钥匙串里的条目迁进加密文件，并删除钥匙串副本。
///
/// 安全顺序：**先写文件 → 回读校验一致 → 才删钥匙串**。
/// 任何一步失败就保留钥匙串条目，绝不丢密钥。
///
/// 返回迁移成功的 account 列表。
pub fn migrate_from_keychain(accounts: &[&str]) -> Result<Vec<String>> {
    let mut migrated = Vec::new();
    for account in accounts {
        if has(account) {
            continue;
        }
        let Some(secret) = read_keychain(account) else {
            continue;
        };
        store(account, &secret)?;

        // 回读校验：文件里必须能解出同一个值，否则不删钥匙串
        match read_sealed(account) {
            Some(back) if back == secret => {
                delete_keychain(account)?;
                migrated.push((*account).to_string());
            }
            _ => {
                // 校验不过：把刚写的删掉，钥匙串那份保持原样
                let _ = delete(account);
                return Err(Error::Config(format!(
                    "迁移 `{account}` 校验失败，已保留系统钥匙串中的原条目"
                )));
            }
        }
    }
    Ok(migrated)
}

/// 密钥文件路径（设置界面展示用）。
pub fn secrets_file_path() -> PathBuf {
    secrets_path()
}

// ---------------------------------------------------------------------------
// 老钥匙串（只读 + 删除，用于迁移）
// ---------------------------------------------------------------------------

/// 从 macOS 钥匙串读一个通用密码条目。失败返回 None。
///
/// 走**双通道**：
/// 1. keyring crate —— 自己创建的条目用它没问题；
/// 2. macOS 自带 `security` CLI —— 用 `security add-generic-password` 建的条目，
///    其 ACL 只信任 `/usr/bin/security`，别的二进制直读会被 macOS 隐藏
///    （返回 `errSecItemNotFound`），此时必须借道 `security`。
fn read_keychain(account: &str) -> Option<String> {
    if let Ok(v) = keyring::Entry::new(LEGACY_SERVICE, account).and_then(|e| e.get_password()) {
        if !v.trim().is_empty() {
            return Some(v);
        }
    }
    read_via_security_cli(account)
}

/// 用 macOS 自带 `security` 读一个通用密码条目。失败返回 None。
fn read_via_security_cli(account: &str) -> Option<String> {
    let out = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            LEGACY_SERVICE,
            "-a",
            account,
            "-w",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // `-w` 输出末尾带换行
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let trimmed = text.trim_end_matches(['\n', '\r']);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 删除老钥匙串里的条目（两条通道都试）。
fn delete_keychain(account: &str) -> Result<()> {
    let _ = keyring::Entry::new(LEGACY_SERVICE, account).and_then(|e| e.delete_credential());

    // 用 output() 而不是 status()：security 会把条目元数据打到 stdout
    let out = std::process::Command::new("security")
        .args([
            "delete-generic-password",
            "-s",
            LEGACY_SERVICE,
            "-a",
            account,
        ])
        .output()
        .map_err(|e| Error::Config(format!("调用 security 失败: {e}")))?;
    if !out.status.success() {
        // 条目本来就不存在也算成功
        return Ok(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **密钥绝不能进 git 仓库**（也不该出现在任何提交过的文件里）。
    ///
    /// 检查方式：把 git 仓库里的**每一个 blob** 读出来，在内存里找密钥字节。
    /// 刻意**不用** `git grep -f <file>`：那样会把密钥短暂写进磁盘；
    /// 也**不用**命令行参数（`ps` 能看到）。密钥全程只在内存里。
    ///
    /// 没有密钥 / 不是 git 仓库 / 不是仓库根目录 → 跳过。
    #[test]
    fn secrets_never_enter_the_repository() {
        use std::process::Command;

        // 拿到本机真实密钥（读加密文件，不打印）
        let mut keys = Vec::new();
        for account in ["vox.llm", "vox.stt"] {
            if let Ok(k) = resolve(None, Some(&format!("keyring:{account}"))) {
                if k.len() >= 8 {
                    keys.push((account.to_string(), k));
                }
            }
        }
        if keys.is_empty() {
            return; // 本机没配密钥 → 无从检查
        }

        // 找仓库根目录
        let root = std::env::current_dir().ok().and_then(|mut d| loop {
            if d.join(".git").exists() {
                break Some(d);
            }
            if !d.pop() {
                break None;
            }
        });
        let Some(root) = root else { return };

        // 列出所有提交里的所有对象
        let Ok(list) = Command::new("git")
            .args(["-C", &root.to_string_lossy(), "rev-list", "--all", "--objects"])
            .output()
        else {
            return;
        };
        if !list.status.success() {
            return;
        }
        let text = String::from_utf8_lossy(&list.stdout);
        let mut shas = Vec::new();
        for line in text.lines() {
            let sha = line.split(' ').next().unwrap_or("");
            if sha.len() == 40 {
                shas.push(sha.to_string());
            }
        }
        if shas.is_empty() {
            return; // 还没有提交
        }

        let mut hits: Vec<String> = Vec::new();
        for sha in &shas {
            let Ok(obj) = Command::new("git")
                .args(["-C", &root.to_string_lossy(), "cat-file", "blob", sha])
                .output()
            else {
                continue;
            };
            if obj.stdout.is_empty() {
                continue;
            }
            for (account, key) in &keys {
                if obj.stdout.windows(key.len()).any(|w| w == key.as_bytes()) {
                    hits.push(format!("{account} 出现在 blob {sha}"));
                }
            }
        }
        assert!(
            hits.is_empty(),
            "密钥出现在 git 历史里：{hits:?}\n（密钥不会被打印，只报告位置）"
        );
    }

    #[test]
    fn inline_key_wins() {
        let k = resolve(Some(" inline "), Some("keyring:nope")).unwrap();
        assert_eq!(k, "inline");
    }

    #[test]
    fn missing_config_is_error() {
        assert!(resolve(None, None).is_err());
        assert!(resolve(Some("  "), None).is_err());
    }

    #[test]
    fn strips_prefixes() {
        assert_eq!(account_of("keyring:vox.stt"), "vox.stt");
        assert_eq!(account_of("file:vox.llm"), "vox.llm");
        assert_eq!(account_of("vox.stt"), "vox.stt");
    }

    #[test]
    fn hex_roundtrip() {
        let data = [0u8, 1, 15, 16, 254, 255];
        let enc = hex_encode(&data);
        assert_eq!(hex_decode(&enc).unwrap(), data);
        assert!(hex_decode("xyz").is_none());
        assert!(hex_decode("abc").is_none());
    }

    #[test]
    fn seal_unseal_roundtrip() {
        let key = [7u8; KEY_LEN];
        let sealed = seal(&key, "vox.stt", "sk-secret-value").unwrap();
        let plain = unseal(&key, "vox.stt", &sealed).unwrap();
        assert_eq!(plain, "sk-secret-value");
    }

    #[test]
    fn wrong_account_cannot_decrypt() {
        // AAD 绑定了 account 名：换个条目解不开
        let key = [7u8; KEY_LEN];
        let sealed = seal(&key, "vox.stt", "sk-secret-value").unwrap();
        assert!(unseal(&key, "vox.llm", &sealed).is_err());
    }

    #[test]
    fn wrong_key_cannot_decrypt() {
        let sealed = seal(&[7u8; KEY_LEN], "vox.stt", "sk-secret-value").unwrap();
        assert!(unseal(&[9u8; KEY_LEN], "vox.stt", &sealed).is_err());
    }
}
