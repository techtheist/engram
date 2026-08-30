//! At-rest sealing (0.8.4 for history, 0.9.0 for both stores): the codec,
//! the machine key, and the blind keyword tokens that keep BM25 identical
//! across sealed and plaintext states.
//!
//! Sealing is FIELD-LEVEL, applied by the store driver: `zstd` compress,
//! then XChaCha20-Poly1305, marked with the [`SEAL_PREFIX`]. What stays open
//! by design: node/edge structure, types, timestamps, ids, and embedding
//! vectors (documented inversion risk — vectors recover gist, not text).
//! Whether a given store is sealed is recorded IN that store's meta
//! (`encryption_state`), so the file self-describes and the daemon can never
//! desync from the `.tepin` on disk.

use crate::Result;

/// Sealed-blob marker. Order is fixed: zstd compress, THEN encrypt —
/// ciphertext doesn't compress.
pub const SEAL_PREFIX: &str = "enc1:";

/// What a reader renders when a sealed value can't be opened (no key, wrong
/// key, tampered blob) — a placeholder, never garbage, never an error.
pub const SEALED_PLACEHOLDER: &str = "[sealed — encryption key unavailable]";

pub fn is_sealed(s: &str) -> bool {
    s.starts_with(SEAL_PREFIX)
}

/// A store's recorded at-rest state, persisted in its own meta under
/// `encryption_state`. The two mid-flight states make the whole-store
/// migration crash-resumable: every reader tolerates mixed rows (per-field
/// prefix test), so a killed migration just continues on the next
/// reconcile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncryptionState {
    Plaintext,
    Sealing,
    Sealed,
    Unsealing,
}

impl EncryptionState {
    pub fn as_str(self) -> &'static str {
        match self {
            EncryptionState::Plaintext => "plaintext",
            EncryptionState::Sealing => "sealing",
            EncryptionState::Sealed => "sealed",
            EncryptionState::Unsealing => "unsealing",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "plaintext" => Some(Self::Plaintext),
            "sealing" => Some(Self::Sealing),
            "sealed" => Some(Self::Sealed),
            "unsealing" => Some(Self::Unsealing),
            _ => None,
        }
    }

    /// Do writes seal in this state? (Sealing = migration toward sealed:
    /// new writes seal immediately.)
    pub fn writes_sealed(self) -> bool {
        matches!(self, Self::Sealing | Self::Sealed)
    }
}

/// The machine's sealing key: 256 random bits, minted on first need. Keyring
/// first (macOS Keychain / Windows credential store / secret-service);
/// `~/.engram/history.key` (0600) as the headless fallback, and the only
/// path when `ENGRAM_KEYRING=off`. The keyring entry and file name predate
/// graph sealing (0.8.4's history layer) and are kept verbatim so existing
/// installations keep their key. **No hardware-ID derivation** — hardware
/// ids aren't secret and break on a hardware swap. Key loss = sealed content
/// unreadable; plaintext stores are unaffected.
pub struct SealKey(chacha20poly1305::Key);

impl SealKey {
    pub fn load_or_create() -> Option<Self> {
        let keyring_ok = !std::env::var("ENGRAM_KEYRING").is_ok_and(|v| v == "off");
        if keyring_ok && let Some(k) = Self::from_keyring() {
            return Some(k);
        }
        if let Some(k) = Self::from_file() {
            return Some(k);
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).ok()?;
        let key = Self(chacha20poly1305::Key::clone_from_slice(&bytes));
        if keyring_ok && key.store_keyring(&bytes) {
            return Some(key);
        }
        key.store_file(&bytes).then_some(key)
    }

    /// A throwaway key for tests — never touches the keyring or disk.
    pub fn ephemeral() -> Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| crate::Error::Io(format!("entropy: {e}")))?;
        Ok(Self(chacha20poly1305::Key::clone_from_slice(&bytes)))
    }

    fn from_keyring() -> Option<Self> {
        let entry = keyring::Entry::new("engram", "history-key").ok()?;
        let b64 = entry.get_password().ok()?;
        Self::decode(&b64)
    }

    fn store_keyring(&self, bytes: &[u8; 32]) -> bool {
        use base64::Engine as _;
        keyring::Entry::new("engram", "history-key")
            .and_then(|e| e.set_password(&base64::engine::general_purpose::STANDARD.encode(bytes)))
            .is_ok()
    }

    fn key_file() -> Option<std::path::PathBuf> {
        crate::registry::engram_home().map(|d| d.join("history.key"))
    }

    fn from_file() -> Option<Self> {
        let raw = std::fs::read_to_string(Self::key_file()?).ok()?;
        Self::decode(raw.trim())
    }

    fn store_file(&self, bytes: &[u8; 32]) -> bool {
        use base64::Engine as _;
        let Some(path) = Self::key_file() else {
            return false;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        if std::fs::write(&path, b64).is_err() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        true
    }

    fn decode(b64: &str) -> Option<Self> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
        (bytes.len() == 32).then(|| Self(chacha20poly1305::Key::clone_from_slice(&bytes)))
    }

    /// zstd(level 3) then XChaCha20-Poly1305, random 24-byte nonce stored
    /// alongside: `enc1:<base64(nonce ‖ ciphertext)>`. Already-sealed input
    /// passes through untouched (migrations re-visit rows), and failure
    /// modes never corrupt: no entropy or encrypt error → plaintext out.
    pub fn seal(&self, plain: &str) -> String {
        use base64::Engine as _;
        use chacha20poly1305::aead::Aead;
        use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
        if is_sealed(plain) {
            return plain.to_string();
        }
        let compressed =
            zstd::bulk::compress(plain.as_bytes(), 3).unwrap_or_else(|_| plain.as_bytes().to_vec());
        let mut nonce_bytes = [0u8; 24];
        if getrandom::fill(&mut nonce_bytes).is_err() {
            return plain.to_string(); // no entropy, no seal — never corrupt
        }
        let nonce = XNonce::from_slice(&nonce_bytes);
        let cipher = XChaCha20Poly1305::new(&self.0);
        match cipher.encrypt(nonce, compressed.as_ref()) {
            Ok(ct) => {
                let mut blob = nonce_bytes.to_vec();
                blob.extend(ct);
                format!(
                    "{SEAL_PREFIX}{}",
                    base64::engine::general_purpose::STANDARD.encode(blob)
                )
            }
            Err(_) => plain.to_string(),
        }
    }

    /// `None` on wrong key / corrupt blob — callers render a placeholder,
    /// never garbage.
    pub fn unseal(&self, sealed: &str) -> Option<String> {
        use base64::Engine as _;
        use chacha20poly1305::aead::Aead;
        use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
        let b64 = sealed.strip_prefix(SEAL_PREFIX)?;
        let blob = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
        if blob.len() < 25 {
            return None;
        }
        let (nonce_bytes, ct) = blob.split_at(24);
        let cipher = XChaCha20Poly1305::new(&self.0);
        let compressed = cipher.decrypt(XNonce::from_slice(nonce_bytes), ct).ok()?;
        let plain = zstd::stream::decode_all(compressed.as_slice()).ok()?;
        String::from_utf8(plain).ok()
    }

    /// Blind keyword token: keyed HMAC-SHA256 of one BM25 term, truncated to
    /// 12 bytes and hex-encoded (24 lowercase-alphanumeric chars, so tepin's
    /// tokenizer passes it through verbatim). Same term → same token, so
    /// term frequencies and document lengths — everything BM25 reads — are
    /// preserved exactly; the index just never holds vocabulary.
    pub fn kw_token(&self, term: &str) -> String {
        use hmac::{Hmac, Mac};
        let mut mac = <Hmac<sha2::Sha256> as Mac>::new_from_slice(self.0.as_slice())
            .expect("HMAC accepts any key length");
        mac.update(b"kw:");
        mac.update(term.as_bytes());
        let digest = mac.finalize().into_bytes();
        digest[..12].iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_roundtrip_and_double_seal_guard() {
        let k = SealKey::ephemeral().unwrap();
        let sealed = k.seal("the launch code is swordfish");
        assert!(is_sealed(&sealed));
        assert_eq!(
            k.unseal(&sealed).as_deref(),
            Some("the launch code is swordfish")
        );
        // Re-sealing a sealed blob is a no-op — migrations re-visit rows.
        assert_eq!(k.seal(&sealed), sealed);
        // A different key can't open it.
        let other = SealKey::ephemeral().unwrap();
        assert_eq!(other.unseal(&sealed), None);
    }

    #[test]
    fn kw_tokens_are_stable_keyed_and_tokenizer_safe() {
        let k = SealKey::ephemeral().unwrap();
        let a = k.kw_token("sqlite");
        assert_eq!(a, k.kw_token("sqlite"), "deterministic per key");
        assert_ne!(a, k.kw_token("postgres"));
        assert_eq!(a.len(), 24);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        let other = SealKey::ephemeral().unwrap();
        assert_ne!(
            a,
            other.kw_token("sqlite"),
            "keyed — no cross-store rainbow tables"
        );
    }
}
