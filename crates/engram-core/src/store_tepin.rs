//! The TepinDB [`Store`] driver (PLAN §7C step 5) — Engram's graph on the
//! sibling project's primitives tier: one `.tepin` file holds the documents
//! (`nodes` / `edges` / `suspects` / `audit` / `meta` collections), the BM25
//! keyword index over the same fields the SQLite FTS mirror covered, and
//! manual-mode vectors (one vector per node, Engram's own embedder — tepin's
//! bundled model stays off via `default-features = false`).
//!
//! Where SQLite answered with SQL, this driver answers with `find` + plain
//! Rust; the shared composites (hybrid fusion, traversal, decay filtering)
//! come from the trait's provided methods, so ranking behavior is identical
//! across backends by construction.

use std::path::Path;

use serde_json::{Value, json};
use tepin_core::ServeMode;
use tepindb::{BatchOp, Db};

use std::sync::{Arc, RwLock};

use crate::config::{GraphConfig, PolicyConfig};
use crate::rag::DEFAULT_EMBED_MODEL;
use crate::seal::{EncryptionState, SEALED_PLACEHOLDER, SealKey, is_sealed};
use crate::store::{SNIPPET_CLOSE, SNIPPET_OPEN, Store, normalize_tags, now};
use crate::types::*;
use crate::{Error, Result};

const NODES: &str = "nodes";
const EDGES: &str = "edges";
const SUSPECTS: &str = "suspects";
const AUDIT: &str = "audit";
const META: &str = "meta";

/// The single synthetic keyword field (0.9.0): the node's searchable token
/// stream — identity tokens on a plaintext store, keyed HMAC digests on a
/// sealed one. See `TepinStore::kw_field`.
const KW_FIELD: &str = "_kw";

/// Whether a graph path belongs to this driver (`.tepin` by convention;
/// the migration writes `graph.tepin` next to the old `graph.db`).
pub fn is_tepin_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "tepin")
}

/// One repo's graph in a single `.tepin` file. Since tepin 0.4 the model
/// swap is `reset_embedder` — a metadata operation — so the handle never
/// needs replacing and the struct is just the `Db`.
pub struct TepinStore {
    db: Db,
    /// The parsed per-graph configuration, cached at open and refreshed by
    /// `set_graph_config` — trust hydration reads it on every document.
    cfg: RwLock<Arc<GraphConfig>>,
    /// This store's recorded at-rest state (0.9.0), read from its own meta
    /// at open — every write consults it, so the file can never desync.
    enc: RwLock<EncryptionState>,
    /// The machine sealing key, loaded lazily when the state needs it. None
    /// with a non-plaintext state = key unavailable: reads render the
    /// placeholder, writes stay plaintext (never corrupt).
    key: RwLock<Option<Arc<SealKey>>>,
}

impl TepinStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        // Retry rides out cold-start races (two sessions opening at once);
        // Host makes this handle serve reads to other processes while it
        // holds the lock — `npx tepindb inspect` on a live store works
        // through the sidecar instead of dying on `database_locked`.
        let db = Db::options()
            .retry_for(std::time::Duration::from_secs(3))
            .serve(ServeMode::Host)
            .open(path.as_ref())?;
        let store = Self {
            db,
            cfg: RwLock::new(Arc::new(GraphConfig::default())),
            enc: RwLock::new(EncryptionState::Plaintext),
            key: RwLock::new(None),
        };
        configure(store.db())?;
        store.reload_config()?;
        store.load_encryption()?;
        store.ensure_keyword_fields()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let store = Self {
            db: Db::open_in_memory()?,
            cfg: RwLock::new(Arc::new(GraphConfig::default())),
            enc: RwLock::new(EncryptionState::Plaintext),
            key: RwLock::new(None),
        };
        configure(store.db())?;
        store.reload_config()?;
        store.ensure_keyword_fields()?;
        Ok(store)
    }

    /// Re-parse the stored config document into the cache.
    fn reload_config(&self) -> Result<()> {
        let raw = Store::graph_config(self)?;
        *self.cfg.write().unwrap() = Arc::new(GraphConfig::from_stored(raw.as_deref()));
        Ok(())
    }

    /// The live policy numbers, cloned out of the cached config for document
    /// hydration.
    fn policy(&self) -> PolicyConfig {
        self.cfg.read().unwrap().policy.clone()
    }

    fn db(&self) -> &Db {
        &self.db
    }

    fn get_doc(&self, collection: &str, id: &str) -> Result<Option<Value>> {
        no_collection_is_empty(self.db().get(collection, id))
    }

    fn find_docs(&self, collection: &str, filter: &Value) -> Result<Vec<Value>> {
        no_collection_is_empty(self.db().find(collection, filter))
    }

    fn get_meta(&self, key: &str) -> Result<Option<Value>> {
        self.get_doc(META, key)
    }

    fn set_meta(&self, key: &str, mut doc: Value) -> Result<()> {
        doc["_id"] = json!(key);
        self.db().upsert(META, doc)?;
        Ok(())
    }

    /// String meta values ride a `{ value }` wrapper doc — one shape for
    /// graph_config, current_version and friends.
    fn meta_str(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .get_meta(key)?
            .and_then(|d| d["value"].as_str().map(str::to_string)))
    }

    fn set_meta_str(&self, key: &str, value: &str) -> Result<()> {
        self.set_meta(key, json!({ "value": value }))
    }

    fn write_node(&self, node: &Node, _exists: bool) -> Result<()> {
        self.db().upsert(NODES, self.doc_for_node(node)?)?;
        Ok(())
    }

    /// The graph's indexed custom-field names, cloned out of the cached
    /// config — their values join the `_kw` keyword text.
    fn indexed_names(&self) -> Vec<String> {
        self.config()
            .indexed_field_names()
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    /// The BM25/keyword source is ONE synthetic field, `_kw` (0.9.0): the
    /// node's searchable token stream, identity tokens on a plaintext store
    /// and keyed HMAC digests on a sealed one. One field in BOTH states =
    /// one code path, and BM25 statistics (term frequencies, doc lengths)
    /// are identical by construction. Re-registering rebuilds the index —
    /// the tepin analog of `ensure_fts`.
    fn ensure_keyword_fields(&self) -> Result<()> {
        let fields = [KW_FIELD];
        let infos = self.db().collections()?;
        let ok = infos
            .iter()
            .find(|c| c.name == NODES)
            .is_some_and(|c| c.manual_vectors && c.embed == fields);
        if !ok {
            self.db().set_manual_vectors(NODES, &fields)?;
        }
        Ok(())
    }

    // ---- at-rest sealing (0.9.0) ----------------------------------------

    /// Read this store's recorded state from its own meta and load the key
    /// when the state needs one. Called at open; never at `open_in_memory`
    /// (tests must not touch the keyring).
    fn load_encryption(&self) -> Result<()> {
        let state = Store::kv_get(self, "encryption_state")?
            .as_deref()
            .and_then(EncryptionState::parse)
            .unwrap_or(EncryptionState::Plaintext);
        *self.enc.write().unwrap() = state;
        if state != EncryptionState::Plaintext && self.seal_key().is_none() {
            *self.key.write().unwrap() = SealKey::load_or_create().map(Arc::new);
        }
        Ok(())
    }

    fn enc_state(&self) -> EncryptionState {
        *self.enc.read().unwrap()
    }

    fn seal_key(&self) -> Option<Arc<SealKey>> {
        self.key.read().unwrap().clone()
    }

    /// Inject a key without touching the keyring — tests only.
    pub fn set_seal_key_for_tests(&self, key: SealKey) {
        *self.key.write().unwrap() = Some(Arc::new(key));
    }

    /// Seal one string for storage under the current state (no-op on a
    /// plaintext store or without a key — never corrupt).
    fn seal_write(&self, s: &str) -> String {
        match (self.enc_state().writes_sealed(), self.seal_key()) {
            (true, Some(k)) => k.seal(s),
            _ => s.to_string(),
        }
    }

    /// Open one stored string for a reader: plaintext passes through, a
    /// sealed blob decrypts, and a blob we can't open renders the
    /// placeholder — never garbage, never an error.
    fn unseal_read(&self, s: &str) -> String {
        if !is_sealed(s) {
            return s.to_string();
        }
        self.seal_key()
            .and_then(|k| k.unseal(s))
            .unwrap_or_else(|| SEALED_PLACEHOLDER.to_string())
    }

    /// The `_kw` token stream for a node: tokens of title/body/tags/
    /// code_refs plus the values of indexed custom fields, run through the
    /// state's term transform (identity or keyed HMAC).
    fn kw_field(&self, n: &Node) -> String {
        let mut text = n.title.clone();
        if let Some(b) = &n.body {
            text.push(' ');
            text.push_str(b);
        }
        if !n.tags.is_empty() {
            text.push(' ');
            text.push_str(&n.tags.join(" "));
        }
        if !n.code_refs.is_empty() {
            text.push(' ');
            text.push_str(&n.code_refs.join(" "));
        }
        if let Some(fields) = &n.fields {
            for name in self.indexed_names() {
                if let Some(v) = fields.get(&name) {
                    text.push(' ');
                    match v {
                        Value::String(s) => text.push_str(s),
                        other => text.push_str(&other.to_string()),
                    }
                }
            }
        }
        self.kw_transform_tokens(tokenize(&text))
    }

    /// Transform already-tokenized terms for the keyword index / a query:
    /// identity on plaintext, keyed HMAC digests when writes seal.
    fn kw_transform_tokens(&self, tokens: Vec<String>) -> String {
        match (self.enc_state().writes_sealed(), self.seal_key()) {
            (true, Some(k)) => tokens
                .iter()
                .map(|t| k.kw_token(t))
                .collect::<Vec<_>>()
                .join(" "),
            _ => tokens.join(" "),
        }
    }

    /// Node → stored document. Beside the serde image: computed members
    /// stripped, `_id` stamped, the `_kw` keyword stream attached, and —
    /// when the state seals writes — title/body sealed and the structured
    /// members (tags, code_refs, fields, props) folded into sealed JSON
    /// strings. `node_from_doc` reverses all of it.
    fn doc_for_node(&self, n: &Node) -> Result<Value> {
        let mut doc = serde_json::to_value(n)?;
        let obj = doc.as_object_mut().expect("node serializes to an object");
        obj.remove("trust");
        obj.remove("stale");
        obj.insert("_id".into(), json!(n.id));
        obj.insert(KW_FIELD.into(), json!(self.kw_field(n)));
        if self.enc_state().writes_sealed() && self.seal_key().is_some() {
            obj.insert("title".into(), json!(self.seal_write(&n.title)));
            if let Some(b) = &n.body {
                obj.insert("body".into(), json!(self.seal_write(b)));
            }
            for member in ["tags", "code_refs", "fields", "props"] {
                if let Some(v) = obj.get(member).filter(|v| !v.is_null()) {
                    let raw = serde_json::to_string(v)?;
                    obj.insert(member.into(), json!(self.seal_write(&raw)));
                }
            }
        }
        Ok(doc)
    }

    /// Stored document → node: sealed members open before deserialization
    /// (their JSON types differ sealed vs plain), then trust hydrates.
    fn node_from_doc(&self, mut doc: Value, policy: &PolicyConfig) -> Result<Node> {
        if let Some(obj) = doc.as_object_mut() {
            for member in ["title", "body"] {
                if let Some(Value::String(s)) = obj.get(member)
                    && is_sealed(s)
                {
                    let open = self.unseal_read(s);
                    obj.insert(member.into(), json!(open));
                }
            }
            for member in ["tags", "code_refs", "fields", "props"] {
                if let Some(Value::String(s)) = obj.get(member)
                    && is_sealed(s)
                {
                    match self
                        .seal_key()
                        .and_then(|k| k.unseal(s))
                        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                    {
                        Some(restored) => {
                            obj.insert(member.into(), restored);
                        }
                        // Unopenable (no key): drop the member so serde's
                        // defaults apply — empty lists, absent maps.
                        None => {
                            obj.remove(member);
                        }
                    }
                }
            }
        }
        let mut n: Node = serde_json::from_value(doc)?;
        n.trust = crate::policy::trust(&n.trust_inputs(), now(), policy);
        n.stale = crate::policy::is_stale(n.trust, policy);
        Ok(n)
    }

    /// Edge → stored document (the free-text `note` seals like a body).
    fn doc_for_edge(&self, e: &Edge) -> Result<Value> {
        let mut doc = serde_json::to_value(e)?;
        doc["_id"] = json!(e.id);
        if let Some(note) = &e.note
            && self.enc_state().writes_sealed()
        {
            doc["note"] = json!(self.seal_write(note));
        }
        Ok(doc)
    }

    fn edge_from_doc(&self, mut doc: Value) -> Result<Edge> {
        if let Some(Value::String(s)) = doc.get("note")
            && is_sealed(s)
        {
            let open = self.unseal_read(s);
            doc["note"] = json!(open);
        }
        Ok(serde_json::from_value(doc)?)
    }

    /// Seal/unseal an audit row's payload members (`title`, `before`,
    /// `after` — the before/after images carry full node JSON, which would
    /// otherwise leak every sealed node's plaintext history).
    fn audit_doc_recode(&self, mut doc: Value) -> Result<Value> {
        let Some(obj) = doc.as_object_mut() else {
            return Ok(doc);
        };
        for member in ["title", "before", "after"] {
            let Some(v) = obj.get(member).filter(|v| !v.is_null()) else {
                continue;
            };
            // Open whatever is there first…
            let open: Value = match v {
                Value::String(s) if is_sealed(s) => match self.seal_key().and_then(|k| k.unseal(s))
                {
                    Some(raw) => {
                        if member == "title" {
                            json!(raw)
                        } else {
                            serde_json::from_str(&raw).unwrap_or(Value::Null)
                        }
                    }
                    None => continue, // can't open — leave the row as-is
                },
                other => other.clone(),
            };
            // …then re-store it under the current state.
            let stored = if self.enc_state().writes_sealed() && self.seal_key().is_some() {
                let raw = match &open {
                    Value::String(s) if member == "title" => s.clone(),
                    other => serde_json::to_string(other)?,
                };
                json!(self.seal_write(&raw))
            } else {
                open
            };
            obj.insert(member.into(), stored);
        }
        Ok(doc)
    }

    /// Audit row read path: open sealed members for the reader.
    fn audit_from_doc(&self, mut doc: Value) -> Result<AuditEntry> {
        if let Some(obj) = doc.as_object_mut() {
            if let Some(Value::String(s)) = obj.get("title")
                && is_sealed(s)
            {
                let open = self.unseal_read(s);
                obj.insert("title".into(), json!(open));
            }
            for member in ["before", "after"] {
                if let Some(Value::String(s)) = obj.get(member)
                    && is_sealed(s)
                {
                    let restored = self
                        .seal_key()
                        .and_then(|k| k.unseal(s))
                        .and_then(|raw| serde_json::from_str(&raw).ok())
                        .unwrap_or(Value::Null);
                    obj.insert(member.into(), restored);
                }
            }
        }
        Ok(serde_json::from_value(doc)?)
    }

    fn edges_matching(&self, field: &str, id: &str) -> Result<Vec<Edge>> {
        self.find_docs(EDGES, &json!({ field: id }))?
            .into_iter()
            .map(|d| self.edge_from_doc(d))
            .collect()
    }

    fn suspects_matching(&self, filter: Value) -> Result<Vec<Suspect>> {
        self.find_docs(SUSPECTS, &filter)?
            .into_iter()
            .map(doc_suspect)
            .collect()
    }

    /// The `model_id` stamped on stored vectors — the recorded identity, or
    /// the default model for stores that predate model selection.
    fn vector_model_id(&self) -> Result<String> {
        Ok(Store::embed_model(self)?
            .map(|m| m.name)
            .unwrap_or_else(|| DEFAULT_EMBED_MODEL.to_string()))
    }
}

/// Idempotent per-open setup: manual-vector mode + keyword fields on `nodes`,
/// endpoint indexes on `edges`, self-describing purposes. Guarded by what
/// `collections()` already reports — reconfiguring rebuilds indexes, so an
/// already-configured file must pay nothing.
fn configure(db: &Db) -> Result<()> {
    let infos = db.collections()?;
    let info = |name: &str| infos.iter().find(|c| c.name == name);

    let nodes_ok = info(NODES).is_some_and(|c| c.manual_vectors && c.embed == [KW_FIELD]);
    if !nodes_ok {
        db.set_manual_vectors(NODES, &[KW_FIELD])?;
    }
    for field in ["from_id", "to_id"] {
        if !info(EDGES).is_some_and(|c| c.indexes.iter().any(|i| i == field)) {
            db.create_index(EDGES, field)?;
        }
    }
    let purposes = [
        (
            NODES,
            "Engram memory nodes — typed reasoning/decision knowledge; one doc per node, one vector per node (manual mode, Engram's embedder)",
        ),
        (
            EDGES,
            "Sentence-shaped links between nodes (because/answers/replaces/conflicts-with/…); indexed by from_id and to_id",
        ),
        (
            SUSPECTS,
            "Suspected-conflict queue: unlinked look-alike node pairs awaiting a judgment",
        ),
        (
            AUDIT,
            "Append-only mutation journal; _id is the zero-padded seq",
        ),
        (
            META,
            "Store-level facts: embed_version, embed_model, audit_seq",
        ),
    ];
    for (name, purpose) in purposes {
        if info(name).is_none_or(|c| c.purpose.is_none()) {
            db.set_purpose(name, purpose)?;
        }
    }
    Ok(())
}

impl Store for TepinStore {
    // ---- store-level metadata -------------------------------------------

    fn embed_version(&self) -> Result<i64> {
        Ok(self
            .get_meta("embed_version")?
            .and_then(|d| d["value"].as_i64())
            .unwrap_or(0))
    }

    fn set_embed_version(&self, v: i64) -> Result<()> {
        self.set_meta("embed_version", json!({ "value": v }))
    }

    fn embed_model(&self) -> Result<Option<EmbedModelId>> {
        match self.get_meta("embed_model")? {
            Some(doc) => Ok(Some(EmbedModelId {
                name: doc["name"].as_str().unwrap_or_default().to_string(),
                dim: doc["dim"].as_u64().unwrap_or(0) as usize,
            })),
            None => Ok(None),
        }
    }

    fn set_embed_model(&self, model: &EmbedModelId) -> Result<()> {
        self.set_meta(
            "embed_model",
            json!({ "name": model.name, "dim": model.dim }),
        )
    }

    fn graph_config(&self) -> Result<Option<String>> {
        self.meta_str("graph_config")
    }

    fn set_graph_config(&self, json: &str) -> Result<()> {
        self.set_meta_str("graph_config", json)?;
        *self.cfg.write().unwrap() = Arc::new(GraphConfig::from_stored(Some(json)));
        // A changed indexed-field set re-registers the keyword fields (tepin
        // rebuilds the BM25 index); existing docs re-hoist on their next
        // write — the engine re-embeds them in the same gesture.
        self.ensure_keyword_fields()?;
        Ok(())
    }

    fn config(&self) -> Arc<GraphConfig> {
        self.cfg.read().unwrap().clone()
    }

    fn current_version(&self) -> Result<Option<String>> {
        self.meta_str("current_version")
    }

    fn set_current_version(&self, version: Option<&str>) -> Result<()> {
        match version {
            Some(v) => self.set_meta_str("current_version", v),
            None => {
                // Purposed-but-missing docs read as absent; delete is enough.
                let _ = self.db().delete(META, "current_version");
                Ok(())
            }
        }
    }

    fn kv_get(&self, key: &str) -> Result<Option<String>> {
        self.meta_str(key)
    }

    fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        self.set_meta_str(key, value)
    }

    fn encryption_state(&self) -> EncryptionState {
        self.enc_state()
    }

    fn set_encryption_state(&self, state: EncryptionState) -> Result<()> {
        if state != EncryptionState::Plaintext && self.seal_key().is_none() {
            *self.key.write().unwrap() = SealKey::load_or_create().map(Arc::new);
            if state.writes_sealed() && self.seal_key().is_none() {
                return Err(Error::Config(
                    "no encryption key available — both the OS keyring and the \
                     ~/.engram/history.key fallback failed"
                        .into(),
                ));
            }
        }
        // Persist FIRST: a crash right after leaves the recorded state ahead
        // of the rows, which the mid-flight states are built to tolerate.
        self.set_meta_str("encryption_state", state.as_str())?;
        *self.enc.write().unwrap() = state;
        Ok(())
    }

    fn reseal_all(&self, progress: &mut dyn FnMut(usize, usize)) -> Result<usize> {
        // Reading opens whatever is sealed; writing re-stores under the
        // CURRENT state — one pass serves both directions, is idempotent
        // (double-seal is guarded), and survives being killed anywhere.
        let nodes = self.all_nodes()?;
        let edges = self.all_edges()?;
        let audits = self.find_docs(AUDIT, &json!({}))?;
        let total = nodes.len() + edges.len() + audits.len();
        let mut done = 0usize;
        for n in &nodes {
            self.write_node(n, true)?;
            done += 1;
            progress(done, total);
        }
        for e in &edges {
            self.db().update(EDGES, &e.id, self.doc_for_edge(e)?)?;
            done += 1;
            progress(done, total);
        }
        for doc in audits {
            let Some(id) = doc["_id"].as_str().map(str::to_string) else {
                done += 1;
                continue;
            };
            let recoded = self.audit_doc_recode(doc)?;
            self.db().update(AUDIT, &id, recoded)?;
            done += 1;
            progress(done, total);
        }
        Ok(total)
    }

    fn reset_vectors(&self, _dim: usize) -> Result<()> {
        // tepin 0.4's reset_embedder (an Engram dossier ask): clears the
        // per-file model pin and every stored vector as a metadata operation
        // — documents and the keyword index untouched, no file rebuild. The
        // caller re-embeds immediately after, which re-pins the new model.
        self.db().reset_embedder()?;
        Ok(())
    }

    fn stats(&self) -> Result<StoreStats> {
        let db = self.db();
        let infos = db.collections()?;
        let count = |name: &str| {
            infos
                .iter()
                .find(|c| c.name == name)
                .map(|c| c.count as i64)
                .unwrap_or(0)
        };
        let mut embedded = 0i64;
        for doc in no_collection_is_empty(db.find(NODES, &json!({})))? {
            if let Some(id) = doc["_id"].as_str()
                && !db.get_vectors(NODES, id)?.is_empty()
            {
                embedded += 1;
            }
        }
        Ok(StoreStats {
            backend: "tepindb",
            nodes: count(NODES),
            edges: count(EDGES),
            embedded,
        })
    }

    fn health(&self) -> Result<StoreHealth> {
        // redb validates its checksummed B-tree on open and fsyncs every
        // commit; a corrupt file would have failed Db::open.
        Ok(StoreHealth {
            journal_mode: None,
            integrity_ok: true,
            detail: None,
        })
    }

    // ---- nodes -----------------------------------------------------------

    fn add_node(&self, n: NewNode) -> Result<Node> {
        let id = crate::id::new_id();
        // Same clamp as the SQLite backend: provided dates for historical
        // material, never a future stamp.
        let created = n.created_at.map(|t| t.min(now())).unwrap_or_else(now);
        let node = Node {
            id: id.clone(),
            node_type: n.node_type,
            title: crate::redact::scrub(&n.title),
            body: n.body.as_deref().map(crate::redact::scrub),
            durability: n.durability,
            source: n.source,
            session_id: n.session_id,
            created_at: created,
            valid_from: Some(created),
            valid_until: None,
            status: n.status,
            last_seen: None,
            confirmed_at: None,
            // User-authored knowledge is approved by construction.
            approved_at: (n.source == Source::User).then_some(created),
            demoted_at: None,
            trust_override: None,
            trust: 0.0,
            stale: false,
            code_refs: n.code_refs,
            tags: normalize_tags(&n.tags),
            version: n.version,
            props: n.props,
            fields: n
                .fields
                .as_ref()
                .filter(|f| !f.is_empty())
                .map(crate::redact::scrub_fields),
        };
        self.write_node(&node, false)?;
        self.get_node(&id)?.ok_or(Error::NotFound(id))
    }

    fn get_node(&self, id: &str) -> Result<Option<Node>> {
        self.get_doc(NODES, id)?
            .map(|d| self.node_from_doc(d, &self.policy()))
            .transpose()
    }

    fn update_node(&self, id: &str, p: NodePatch) -> Result<Node> {
        let mut node = self
            .get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        if let Some(v) = p.node_type {
            node.node_type = v;
        }
        if let Some(v) = p.title {
            node.title = crate::redact::scrub(&v);
        }
        if let Some(v) = p.body {
            node.body = Some(crate::redact::scrub(&v));
        }
        if let Some(v) = p.durability {
            node.durability = v;
        }
        if let Some(v) = p.status {
            node.status = Some(v);
        }
        if let Some(v) = p.valid_until {
            node.valid_until = Some(v);
        }
        if let Some(v) = p.code_refs {
            node.code_refs = v;
        }
        if let Some(v) = p.tags {
            node.tags = normalize_tags(&v);
        }
        if let Some(v) = p.version {
            node.version = Some(v);
        }
        // REPLACE semantics by contract: the engine already resolved any
        // merge intent (empty map = clear all custom fields).
        if let Some(v) = p.fields {
            node.fields = (!v.is_empty()).then(|| crate::redact::scrub_fields(&v));
        }
        // A deliberate update is re-validation: it confirms the node (the
        // unapproved trust anchor) and clears any evidence demotion.
        let ts = now();
        node.last_seen = Some(ts);
        node.confirmed_at = Some(ts);
        node.demoted_at = None;
        self.write_node(&node, true)?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn approve(&self, id: &str) -> Result<Node> {
        let mut node = self
            .get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        let ts = now();
        node.approved_at = Some(ts);
        node.last_seen = Some(ts);
        node.confirmed_at = Some(ts);
        node.demoted_at = None;
        self.write_node(&node, true)?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn revoke_approval(&self, id: &str) -> Result<Node> {
        let mut node = self
            .get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        node.approved_at = None;
        node.trust_override = None;
        self.write_node(&node, true)?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn set_trust_override(&self, id: &str, value: Option<f64>) -> Result<Node> {
        let mut node = self
            .get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        node.trust_override = value.map(|v| v.clamp(0.0, 1.0));
        self.write_node(&node, true)?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn demote(&self, id: &str, ts: i64) -> Result<bool> {
        let Some(mut node) = self.get_node(id)? else {
            return Ok(false);
        };
        if node.demoted_at.is_some() || node.trust_override.is_some() {
            return Ok(false);
        }
        node.demoted_at = Some(ts);
        self.write_node(&node, true)?;
        Ok(true)
    }

    fn clear_demotion(&self, id: &str) -> Result<Node> {
        let mut node = self
            .get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        node.demoted_at = None;
        self.write_node(&node, true)?;
        self.get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn delete_node(&self, id: &str) -> Result<bool> {
        let db = self.db();
        if no_collection_is_empty(db.get(NODES, id))?.is_none() {
            return Ok(false);
        }
        // Cascade edges and suspects in the same atomic batch (SQLite did
        // this in one transaction). Deleting the doc drops its vectors and
        // index entries inside tepin.
        let mut ops = vec![BatchOp::Delete {
            collection: NODES.into(),
            id: id.into(),
        }];
        let mut edge_ids: Vec<String> = Vec::new();
        for field in ["from_id", "to_id"] {
            for doc in no_collection_is_empty(db.find(EDGES, &json!({ field: id })))? {
                if let Some(eid) = doc["_id"].as_str()
                    && !edge_ids.iter().any(|e| e == eid)
                {
                    edge_ids.push(eid.to_string());
                }
            }
        }
        ops.extend(edge_ids.into_iter().map(|eid| BatchOp::Delete {
            collection: EDGES.into(),
            id: eid,
        }));
        for field in ["a_id", "b_id"] {
            for doc in no_collection_is_empty(db.find(SUSPECTS, &json!({ field: id })))? {
                if let Some(sid) = doc["_id"].as_str() {
                    ops.push(BatchOp::Delete {
                        collection: SUSPECTS.into(),
                        id: sid.to_string(),
                    });
                }
            }
        }
        db.batch(ops)?;
        Ok(true)
    }

    fn upsert_node(&self, n: &Node) -> Result<()> {
        // Still re-runs redaction (defense in depth), like the SQLite upsert.
        let mut node = n.clone();
        node.title = crate::redact::scrub(&node.title);
        node.body = node.body.as_deref().map(crate::redact::scrub);
        node.tags = normalize_tags(&node.tags);
        node.fields = node.fields.as_ref().map(crate::redact::scrub_fields);
        let exists = self.get_doc(NODES, &node.id)?.is_some();
        self.write_node(&node, exists)
    }

    fn all_nodes(&self) -> Result<Vec<Node>> {
        let mut out: Vec<Node> = self
            .find_docs(NODES, &json!({}))?
            .into_iter()
            .map({
                let policy = self.policy();
                move |d| self.node_from_doc(d, &policy)
            })
            .collect::<Result<_>>()?;
        out.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        Ok(out)
    }

    fn touch(&self, ids: &[String]) -> Result<()> {
        let ts = now();
        for id in ids {
            if let Some(mut node) = self.get_node(id)? {
                node.last_seen = Some(ts);
                self.write_node(&node, true)?;
            }
        }
        Ok(())
    }

    fn backdate_node(&self, id: &str, created_at: i64) -> Result<()> {
        let mut node = self
            .get_node(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        node.created_at = created_at;
        node.valid_from = Some(created_at);
        self.write_node(&node, true)
    }

    // ---- edges -----------------------------------------------------------

    fn add_edge(&self, e: NewEdge) -> Result<Edge> {
        // SQLite enforced the edges→nodes FK; here the driver does.
        for endpoint in [&e.from_id, &e.to_id] {
            if self.get_doc(NODES, endpoint)?.is_none() {
                return Err(Error::NotFound(endpoint.clone()));
            }
        }
        let id = crate::id::new_id();
        let created = now();
        let edge = Edge {
            id: id.clone(),
            edge_type: e.edge_type,
            from_id: e.from_id,
            to_id: e.to_id,
            source: e.source,
            created_at: created,
            confidence: e.confidence,
            strength: e.strength,
            note: e.note,
            valid_from: Some(created),
            valid_until: None,
            status: e.status,
        };
        self.db().insert(EDGES, self.doc_for_edge(&edge)?)?;
        self.get_edge(&id)?.ok_or(Error::NotFound(id))
    }

    fn get_edge(&self, id: &str) -> Result<Option<Edge>> {
        self.get_doc(EDGES, id)?
            .map(|d| self.edge_from_doc(d))
            .transpose()
    }

    fn update_edge(&self, id: &str, p: EdgePatch) -> Result<Edge> {
        let mut edge = self
            .get_edge(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        if let Some(v) = p.edge_type {
            edge.edge_type = v;
        }
        if let Some(v) = p.status {
            edge.status = Some(v);
        }
        if let Some(v) = p.note {
            edge.note = Some(v);
        }
        if let Some(v) = p.confidence {
            edge.confidence = Some(v);
        }
        if let Some(v) = p.strength {
            edge.strength = Some(v);
        }
        self.db().update(EDGES, id, self.doc_for_edge(&edge)?)?;
        self.get_edge(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn delete_edge(&self, id: &str) -> Result<bool> {
        let db = self.db();
        if no_collection_is_empty(db.get(EDGES, id))?.is_none() {
            return Ok(false);
        }
        db.delete(EDGES, id)?;
        Ok(true)
    }

    fn upsert_edge(&self, e: &Edge) -> Result<()> {
        self.db().upsert(EDGES, self.doc_for_edge(e)?)?;
        Ok(())
    }

    fn edges_out(&self, node_id: &str) -> Result<Vec<Edge>> {
        self.edges_matching("from_id", node_id)
    }

    fn edges_in(&self, node_id: &str) -> Result<Vec<Edge>> {
        self.edges_matching("to_id", node_id)
    }

    fn all_edges(&self) -> Result<Vec<Edge>> {
        self.find_docs(EDGES, &json!({}))?
            .into_iter()
            .map(|d| self.edge_from_doc(d))
            .collect()
    }

    // ---- bulk ------------------------------------------------------------

    fn import_raw(&self, nodes: &[Node], edges: &[Edge]) -> Result<()> {
        // One atomic multi-collection batch of native upserts (tepin 0.4),
        // nodes before edges — no per-document existence pre-pass.
        let db = self.db();
        let mut ops: Vec<BatchOp> = Vec::with_capacity(nodes.len() + edges.len());
        for n in nodes {
            let mut node = n.clone();
            node.title = crate::redact::scrub(&node.title);
            node.body = node.body.as_deref().map(crate::redact::scrub);
            node.tags = normalize_tags(&node.tags);
            node.fields = node.fields.as_ref().map(crate::redact::scrub_fields);
            ops.push(BatchOp::Upsert {
                collection: NODES.into(),
                doc: self.doc_for_node(&node)?,
            });
        }
        for e in edges {
            ops.push(BatchOp::Upsert {
                collection: EDGES.into(),
                doc: self.doc_for_edge(e)?,
            });
        }
        if !ops.is_empty() {
            db.batch(ops)?;
        }
        Ok(())
    }

    fn archive_nodes(&self, ids: &[String], ts: i64) -> Result<()> {
        let db = self.db();
        let mut ops: Vec<BatchOp> = Vec::new();
        for id in ids {
            if let Some(node) = self.get_node(id)?
                && node.valid_until.is_none()
            {
                let mut node = node;
                node.valid_until = Some(ts);
                ops.push(BatchOp::Update {
                    collection: NODES.into(),
                    id: id.clone(),
                    doc: self.doc_for_node(&node)?,
                });
            }
        }
        if !ops.is_empty() {
            db.batch(ops)?;
        }
        Ok(())
    }

    // ---- search primitives ----------------------------------------------

    fn search_fts(&self, query: &str, types: &[NodeType], limit: usize) -> Result<Vec<SearchHit>> {
        let terms = tokenize(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        // The stored `_kw` stream went through the state's term transform;
        // the query takes the identical transform, so BM25 scores match the
        // plaintext ranking exactly (0.9.0 blind index).
        let kw_query = self.kw_transform_tokens(terms.clone());
        // Over-fetch: archived/off-type hits fall out below.
        let raw = self
            .db()
            .keyword_search(Some(NODES), &kw_query, limit * 4 + 16)?;
        let mut out = Vec::new();
        for hit in raw {
            let Some(node) = self.get_node(&hit.id)? else {
                continue;
            };
            if node.valid_until.is_some() {
                continue;
            }
            if !types.is_empty() && !types.contains(&node.node_type) {
                continue;
            }
            out.push(SearchHit {
                id: node.id.clone(),
                node_type: node.node_type.clone(),
                title: node.title.clone(),
                snippet: make_snippet(&node, &terms),
                score: hit.score as f64,
                durability: node.durability,
                status: node.status,
                created_at: node.created_at,
                trust: node.trust,
                stale: node.stale,
                session_id: node.session_id.clone(),
                tombstone: false,
                neighbors: Vec::new(),
                project: None,
            });
            if out.len() == limit {
                break;
            }
        }
        Ok(out)
    }

    fn search_vec(&self, query: &[f32], k: usize) -> Result<Vec<(String, f64)>> {
        let hits = self.db().search_by_vector(Some(NODES), query, k)?;
        Ok(hits
            .into_iter()
            .map(|h| (h.id, (1.0 - h.score as f64).clamp(0.0, 2.0)))
            .collect())
    }

    fn upsert_embeddings(&self, node_id: &str, vectors: &[Vec<f32>]) -> Result<()> {
        // TepinDB's chunk model natively: chunk 0 = the node-level vector,
        // chunks 1..N the claims; search_by_vector already scores per-doc
        // best-chunk, so claim-level recall needs nothing else here.
        let model_id = self.vector_model_id()?;
        self.db().set_vectors(NODES, node_id, &model_id, vectors)?;
        Ok(())
    }

    fn embedding_of(&self, node_id: &str) -> Result<Option<Vec<f32>>> {
        Ok(self.db().get_vectors(NODES, node_id)?.into_iter().next())
    }

    // ---- suspects --------------------------------------------------------

    fn suspect_between(&self, a: &str, b: &str) -> Result<bool> {
        Ok(!self
            .suspects_matching(json!({ "a_id": a, "b_id": b }))?
            .is_empty()
            || !self
                .suspects_matching(json!({ "a_id": b, "b_id": a }))?
                .is_empty())
    }

    fn add_suspect(
        &self,
        a_id: &str,
        b_id: &str,
        similarity: f64,
        hint: Option<(&str, f64, Option<&str>)>,
    ) -> Result<Suspect> {
        let id = crate::id::new_id();
        let suspect = Suspect {
            id: id.clone(),
            a_id: a_id.to_string(),
            b_id: b_id.to_string(),
            similarity,
            created_at: now(),
            status: SuspectStatus::Suspected,
            nli_label: hint.map(|(l, _, _)| l.to_string()),
            nli_score: hint.map(|(_, s, _)| s),
            nli_direction: hint.and_then(|(_, _, d)| d.map(str::to_string)),
        };
        let mut doc = serde_json::to_value(&suspect)?;
        doc["_id"] = json!(id);
        self.db().insert(SUSPECTS, doc)?;
        self.get_suspect(&id)?.ok_or(Error::NotFound(id))
    }

    fn get_suspect(&self, id: &str) -> Result<Option<Suspect>> {
        self.get_doc(SUSPECTS, id)?.map(doc_suspect).transpose()
    }

    fn set_suspect_status(&self, id: &str, status: SuspectStatus) -> Result<Suspect> {
        let mut suspect = self
            .get_suspect(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        suspect.status = status;
        let mut doc = serde_json::to_value(&suspect)?;
        doc["_id"] = json!(id);
        self.db().update(SUSPECTS, id, doc)?;
        self.get_suspect(id)?
            .ok_or_else(|| Error::NotFound(id.to_string()))
    }

    fn suspects_pending(&self) -> Result<Vec<SuspectView>> {
        let mut pending =
            self.suspects_matching(json!({ "status": SuspectStatus::Suspected.as_str() }))?;
        pending.sort_by(|a, b| (b.created_at, &b.id).cmp(&(a.created_at, &a.id)));
        let mut out = Vec::new();
        for s in pending {
            let (Some(a), Some(b)) = (self.get_node(&s.a_id)?, self.get_node(&s.b_id)?) else {
                continue;
            };
            // Pairs with an archived endpoint drop out — superseding one
            // side settles the question.
            if a.valid_until.is_some() || b.valid_until.is_some() {
                continue;
            }
            out.push(SuspectView {
                id: s.id,
                similarity: s.similarity,
                created_at: s.created_at,
                nli_label: s.nli_label,
                nli_score: s.nli_score,
                nli_direction: s.nli_direction,
                a: SuspectEndpoint {
                    id: a.id,
                    node_type: a.node_type,
                    title: a.title,
                },
                b: SuspectEndpoint {
                    id: b.id,
                    node_type: b.node_type,
                    title: b.title,
                },
            });
        }
        Ok(out)
    }

    fn all_suspects(&self) -> Result<Vec<Suspect>> {
        let mut out = self.suspects_matching(json!({}))?;
        out.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        Ok(out)
    }

    fn upsert_suspect(&self, s: &Suspect) -> Result<()> {
        let mut doc = serde_json::to_value(s)?;
        doc["_id"] = json!(s.id);
        self.db().upsert(SUSPECTS, doc)?;
        Ok(())
    }

    // ---- audit journal ---------------------------------------------------

    fn add_audit(&self, e: &AuditEntry) -> Result<()> {
        let db = self.db();
        let seq = self
            .get_meta("audit_seq")?
            .and_then(|d| d["value"].as_i64())
            .unwrap_or(0)
            + 1;
        let counter = match self.get_meta("audit_seq")?.is_some() {
            true => BatchOp::Update {
                collection: META.into(),
                id: "audit_seq".into(),
                doc: json!({ "_id": "audit_seq", "value": seq }),
            },
            false => BatchOp::Insert {
                collection: META.into(),
                doc: json!({ "_id": "audit_seq", "value": seq }),
            },
        };
        let mut doc = self.audit_doc_recode(serde_json::to_value(e)?)?;
        doc["seq"] = json!(seq);
        // Zero-padded seq as _id keeps journal rows naturally sorted.
        doc["_id"] = json!(format!("{seq:012}"));
        db.batch(vec![
            counter,
            BatchOp::Insert {
                collection: AUDIT.into(),
                doc,
            },
        ])?;
        Ok(())
    }

    fn audit_page(
        &self,
        before: Option<i64>,
        entity_id: Option<&str>,
        limit: usize,
    ) -> Result<AuditPage> {
        let filter = match entity_id {
            Some(eid) => json!({ "entity_id": eid }),
            None => json!({}),
        };
        let mut entries: Vec<AuditEntry> = self
            .find_docs(AUDIT, &filter)?
            .into_iter()
            .map(|doc| self.audit_from_doc(doc))
            .collect::<Result<_>>()?;
        let total = entries.len() as i64;
        entries.sort_by_key(|e| std::cmp::Reverse(e.seq));
        if let Some(before) = before {
            entries.retain(|e| e.seq < before);
        }
        entries.truncate(limit);
        Ok(AuditPage { entries, total })
    }

    // ---- tags ------------------------------------------------------------

    fn tag_stats(&self, limit: usize) -> Result<Vec<TagStat>> {
        use std::collections::HashMap;
        let mut stats: HashMap<String, (i64, i64)> = HashMap::new();
        for node in self.all_nodes()? {
            if node.valid_until.is_some() {
                continue;
            }
            let freshness = node.last_seen.unwrap_or(node.created_at);
            for tag in &node.tags {
                let entry = stats.entry(tag.clone()).or_insert((0, 0));
                entry.0 += 1;
                entry.1 = entry.1.max(freshness);
            }
        }
        let mut out: Vec<TagStat> = stats
            .into_iter()
            .map(|(tag, (count, last_used))| TagStat {
                tag,
                count,
                last_used,
            })
            .collect();
        out.sort_by_key(|t| std::cmp::Reverse((t.last_used, t.count)));
        out.truncate(limit);
        Ok(out)
    }
}

/// TepinDB creates collections lazily on first insert, so a purposed-but-
/// empty collection errors `collection_not_found` on reads — for this driver
/// that simply means "no documents yet".
fn no_collection_is_empty<T: Default>(r: tepindb::Result<T>) -> Result<T> {
    match r {
        Err(e) if e.code == "collection_not_found" => Ok(T::default()),
        other => Ok(other?),
    }
}

// ---- document mapping ----------------------------------------------------

fn doc_suspect(doc: Value) -> Result<Suspect> {
    Ok(serde_json::from_value(doc)?)
}

// ---- keyword snippets ----------------------------------------------------

/// TepinDB's BM25 tokenization, mirrored: lowercase, split on
/// non-alphanumeric, drop tokens shorter than 2 chars.
fn tokenize(q: &str) -> Vec<String> {
    q.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(str::to_string)
        .collect()
}

/// FTS5's `snippet()` replacement: pick the field with the most matched
/// terms, clip a ~12-word window around the first match, and mark matching
/// words with the sentinel pair the pane/MCP already understand.
fn make_snippet(node: &Node, terms: &[String]) -> String {
    let custom = node
        .fields
        .as_ref()
        .map(|f| {
            f.values()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let fields = [
        node.title.clone(),
        node.body.clone().unwrap_or_default(),
        node.tags.join(" "),
        node.code_refs.join(" "),
        custom,
    ];
    let matches_in = |text: &str| tokenize(text).iter().filter(|t| terms.contains(t)).count();
    let best = fields
        .iter()
        .max_by_key(|f| matches_in(f))
        .filter(|f| matches_in(f) > 0);
    match best {
        Some(text) => highlight(text, terms, 12),
        None => crate::store::excerpt(node),
    }
}

fn highlight(text: &str, terms: &[String], window: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let word_matches = |w: &str| tokenize(w).iter().any(|t| terms.contains(t));
    let first = words.iter().position(|w| word_matches(w)).unwrap_or(0);
    let start = first.saturating_sub(window / 3);
    let end = (start + window).min(words.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    for (i, word) in words[start..end].iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if word_matches(word) {
            out.push(SNIPPET_OPEN);
            out.push_str(word);
            out.push(SNIPPET_CLOSE);
        } else {
            out.push_str(word);
        }
    }
    if end < words.len() {
        out.push('…');
    }
    out
}
