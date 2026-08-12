//! The history layer (0.8.4): coding-assistant chat transcripts as an
//! episodic record BENEATH the curated graph — Session/Message nodes in a
//! sibling `history.tepin` store per project, cross-linked to curated memory
//! by `born-in` provenance, and reachable only through the sectioned search
//! fall-through and the pane's history view.
//!
//! Isolation is physical, not a filter: history nodes live in their own
//! store, so curated search, the brief, drift, decay and the suspect scan
//! never see them. The history store is opened by the [`crate::Engine`] that
//! owns the curated store and is deliberately never registered with the hub —
//! the daemon's librarian sweep can't reach it. History nodes are records,
//! not knowledge: no trust, no staleness, no conflicts.
//!
//! Knobs live in [`crate::config::HistoryConfig`] on the CURATED graph's
//! config (the pane edits it there); this store's own config document only
//! carries the chat ontology below.

use std::path::{Path, PathBuf};

use crate::Result;
use crate::config::{GraphConfig, OntologyConfig, TypeRoles, VerbRoles, hidden_brief, tdef, vdef};
use crate::store::Store;
use crate::types::Durability;

/// The sibling store's file name, always beside the curated store.
pub const HISTORY_STORE_FILE: &str = "history.tepin";

/// Node types of the chat ontology (ontology-as-data, the LongMemEval move —
/// outside the curated 8-type count, in a store of their own).
pub const SESSION_TYPE: &str = "Session";
pub const MESSAGE_TYPE: &str = "Message";

/// Verbs: the chain IS the history.
/// `Message in Session`; `Message next Message` / `Session next Session`;
/// curated node `born-in` Message (provenance, written by the MCP layer).
pub const VERB_IN: &str = "in";
pub const VERB_NEXT: &str = "next";
pub const VERB_BORN_IN: &str = "born-in";

/// One history-layer search hit: a snippet plus the handles to expand it
/// (decision 00bgftfdusll — snippets + handles, the model decides how much
/// raw dialogue to spend context on). Never score-blended with curated hits.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistoryHit {
    pub message_id: String,
    /// The harness's session identity — what `expand_history` takes.
    pub session: String,
    pub session_title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    pub timestamp: i64,
    pub snippet: String,
    pub score: f64,
}

/// One message of an expanded exchange, in conversation order.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistoryMessageView {
    pub message_id: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    pub timestamp: i64,
    pub text: String,
}

/// One session row for the pane's history browser: lanes are ordered by
/// `started`, blocks sized by `messages`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistorySessionView {
    /// The Session node id (pane selection handle).
    pub node_id: String,
    /// The harness's session identity — what messages key on.
    pub session: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    pub started: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended: Option<i64>,
    pub messages: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Where a curated node was born (its `born-in` edge, resolved): enough for
/// the provenance line on a search hit and the pane's history chip.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BornIn {
    pub session: String,
    pub message_id: String,
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
}

/// One curated note born inside a recorded session — the reverse of
/// [`BornIn`], for "what did this conversation leave in memory".
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionNote {
    /// The curated node's id (fetch it with `get_node`).
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub title: String,
    /// The turn of the birth exchange, when the message recorded one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    pub message_id: String,
    pub timestamp: i64,
}

/// Where a project's history store lives: `history.tepin` beside the
/// (resolved) curated store — `.engram/graph.tepin` → `.engram/history.tepin`.
pub fn history_store_path(db: &Path) -> PathBuf {
    let resolved = crate::store::resolve_db_path(db);
    match resolved.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(HISTORY_STORE_FILE),
        _ => PathBuf::from(HISTORY_STORE_FILE),
    }
}

/// The history store's own config: the chat ontology. Session and Message
/// plus the three history verbs; `replaces`/`conflicts-with` ride along
/// dormant only because validation requires exactly one of each role — no
/// scan ever runs here to act on them.
pub fn history_ontology() -> GraphConfig {
    let plain = TypeRoles {
        worklist: false,
        anchor: false,
        rank_prior: 0.0,
        highlight: true,
        // The harvester stamps versions explicitly; nothing auto-stamps here.
        versioned: false,
    };
    GraphConfig {
        ontology: OntologyConfig {
            preset: "history".into(),
            types: vec![
                tdef(
                    SESSION_TYPE,
                    262,
                    "one recorded conversation with a coding assistant",
                    Durability::Stable,
                    plain.clone(),
                    hidden_brief(),
                ),
                tdef(
                    MESSAGE_TYPE,
                    199,
                    "one turn of a recorded conversation",
                    Durability::Stable,
                    plain,
                    hidden_brief(),
                ),
            ],
            verbs: vec![
                vdef(VERB_IN, "Message in Session", VerbRoles::default()),
                vdef(VERB_NEXT, "Message next Message", VerbRoles::default()),
                vdef(
                    VERB_BORN_IN,
                    "Decision born-in Message",
                    VerbRoles::default(),
                ),
                vdef(
                    "replaces",
                    "Session replaces Session",
                    VerbRoles {
                        supersession: true,
                        ..VerbRoles::default()
                    },
                ),
                vdef(
                    "conflicts-with",
                    "Session conflicts-with Session",
                    VerbRoles {
                        contradiction: true,
                        ..VerbRoles::default()
                    },
                ),
            ],
        },
        ..GraphConfig::default()
    }
}

// ---------------------------------------------------------------------------
// at-rest sealing (decision 00bgftfausll, caution 00bgftfbusll)
// ---------------------------------------------------------------------------

/// Sealed-blob marker. Order is fixed: zstd compress, THEN encrypt —
/// ciphertext doesn't compress. What's sealed: Message/Session `title` and
/// `body`. What stays open: node/edge structure, types, timestamps, session
/// metadata, ids, and embedding vectors (documented inversion risk — vectors
/// recover gist, not transcripts). No FTS-visible plaintext survives sealing.
pub const SEAL_PREFIX: &str = "enc1:";

pub fn is_sealed(s: &str) -> bool {
    s.starts_with(SEAL_PREFIX)
}

/// The machine's history key: 256 random bits, minted on first need. Keyring
/// first (macOS Keychain / Windows credential store / secret-service);
/// `~/.engram/history.key` (0600) as the headless fallback, and the only
/// path when `ENGRAM_KEYRING=off`. **No hardware-ID derivation** — hardware
/// ids aren't secret and break on a hardware swap. Key loss = history
/// unreadable; the curated graph is unaffected.
pub struct HistoryKey(chacha20poly1305::Key);

impl HistoryKey {
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
    /// alongside: `enc1:<base64(nonce ‖ ciphertext)>`.
    pub fn seal(&self, plain: &str) -> String {
        use base64::Engine as _;
        use chacha20poly1305::aead::Aead;
        use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
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
}

/// Open (creating if absent) a project's history store and make sure it
/// carries the chat ontology. A store that already has a config keeps it —
/// reopening never rewrites.
pub fn open_history_store(path: &Path) -> Result<Box<dyn Store>> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| crate::Error::Io(format!("creating {}: {e}", dir.display())))?;
    }
    let store = crate::store::open_store(path)?;
    if store.graph_config()?.is_none() {
        store.set_graph_config(&serde_json::to_string(&history_ontology())?)?;
    }
    Ok(store)
}
