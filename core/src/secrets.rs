//! 本地 API Key 加密。
//!
//! 威胁模型：攻击者拿到 SQLite 文件（例如同步盘/备份泄漏），但拿不到进程用户目录。
//! 方案：随机 32 字节主密钥落在 `<app_data>/secrets.key`（首次创建，权限 0600），
//! 每个密文独立 96-bit nonce，ChaCha20-Poly1305 AEAD。
//! 这不是 HSM，但足以挡住"数据库拷走即明文钥匙"这类最常见的事故。

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use rand::RngCore;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

pub struct SecretBox {
    cipher: ChaCha20Poly1305,
    key_path: PathBuf,
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretBox").field("key_path", &self.key_path).finish_non_exhaustive()
    }
}

impl SecretBox {
    /// 打开或初始化密钥库
    pub fn open(dir: &Path) -> Result<Self> {
        let key_path = dir.join("secrets.key");
        let key_bytes = if key_path.exists() {
            let raw = std::fs::read(&key_path)?;
            if raw.len() != 32 {
                return Err(Error::Config(format!("密钥文件长度异常: {}", raw.len())));
            }
            raw
        } else {
            if let Some(parent) = key_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut k = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut k);
            std::fs::write(&key_path, k)?;
            harden_permissions(&key_path);
            k.to_vec()
        };
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key_bytes));
        Ok(SecretBox { cipher, key_path })
    }

    pub fn key_path(&self) -> &Path {
        &self.key_path
    }

    /// 返回 `v1:<base64(nonce||ciphertext)>`
    pub fn seal(&self, plaintext: &str) -> Result<String> {
        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|_| Error::Config("密钥加密失败".into()))?;
        let mut out = Vec::with_capacity(12 + ct.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);
        Ok(format!("v1:{}", B64.encode(out)))
    }

    pub fn open_sealed(&self, sealed: &str) -> Result<String> {
        if sealed.is_empty() {
            return Ok(String::new());
        }
        let body = sealed.strip_prefix("v1:").unwrap_or(sealed);
        let raw = B64.decode(body).map_err(|e| Error::Parse(format!("密文解码失败: {e}")))?;
        if raw.len() < 13 {
            return Err(Error::Parse("密文长度不足".into()));
        }
        let nonce = Nonce::from_slice(&raw[..12]);
        let pt = self
            .cipher
            .decrypt(nonce, &raw[12..])
            .map_err(|_| Error::Auth("密钥解密失败：密钥文件可能已更换".into()))?;
        String::from_utf8(pt).map_err(|e| Error::Parse(format!("明文非 UTF-8: {e}")))
    }
}

#[cfg(unix)]
fn harden_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn harden_permissions(_path: &Path) {
    // Windows：依赖用户目录 ACL（%LOCALAPPDATA% 默认仅当前用户可读）；
    // V2 计划接 DPAPI 做二次绑定。
}

/// 打码展示，供 UI 回显
pub fn mask_key(key: &str) -> String {
    let n = key.chars().count();
    if n <= 8 {
        return "••••".into();
    }
    let head: String = key.chars().take(4).collect();
    let tail: String = key.chars().skip(n - 4).collect();
    format!("{head}••••{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_and_open_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let sb = SecretBox::open(dir.path()).unwrap();
        let sealed = sb.seal("sk-test-1234567890").unwrap();
        assert!(sealed.starts_with("v1:"));
        assert!(!sealed.contains("sk-test"));
        assert_eq!(sb.open_sealed(&sealed).unwrap(), "sk-test-1234567890");
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let sb = SecretBox::open(dir.path()).unwrap();
        let mut sealed = sb.seal("secret").unwrap();
        sealed.push('A');
        assert!(sb.open_sealed(&sealed).is_err());
    }

    #[test]
    fn mask_hides_middle() {
        assert_eq!(mask_key("sk-abcdefghijklmnop"), "sk-a••••mnop");
        assert_eq!(mask_key("short"), "••••");
    }
}
