//! Opening a TepinDB store: collections, encryption state and document codecs.

use super::*;

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
    pub(super) fn reload_config(&self) -> Result<()> {
        let raw = Store::graph_config(self)?;
        *self.cfg.writing() = Arc::new(GraphConfig::from_stored(raw.as_deref()));
        Ok(())
    }

    /// The live policy numbers, cloned out of the cached config for document
    /// hydration.
    pub(super) fn policy(&self) -> PolicyConfig {
        self.cfg.reading().policy.clone()
    }

    pub(super) fn db(&self) -> &Db {
        &self.db
    }

    pub(super) fn get_doc(&self, collection: &str, id: &str) -> Result<Option<Value>> {
        no_collection_is_empty(self.db().get(collection, id))
    }

    pub(super) fn find_docs(&self, collection: &str, filter: &Value) -> Result<Vec<Value>> {
        no_collection_is_empty(self.db().find(collection, filter))
    }

    pub(super) fn get_meta(&self, key: &str) -> Result<Option<Value>> {
        self.get_doc(META, key)
    }

    pub(super) fn set_meta(&self, key: &str, mut doc: Value) -> Result<()> {
        doc["_id"] = json!(key);
        self.db().upsert(META, doc)?;
        Ok(())
    }

    /// String meta values ride a `{ value }` wrapper doc — one shape for
    /// graph_config, current_version and friends.
    pub(super) fn meta_str(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .get_meta(key)?
            .and_then(|d| d["value"].as_str().map(str::to_string)))
    }

    pub(super) fn set_meta_str(&self, key: &str, value: &str) -> Result<()> {
        self.set_meta(key, json!({ "value": value }))
    }

    pub(super) fn write_node(&self, node: &Node, _exists: bool) -> Result<()> {
        self.db().upsert(NODES, self.doc_for_node(node)?)?;
        Ok(())
    }

    /// The graph's indexed custom-field names, cloned out of the cached
    /// config — their values join the `_kw` keyword text.
    pub(super) fn indexed_names(&self) -> Vec<String> {
        self.config()
            .indexed_field_names()
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    /// The BM25/keyword source is ONE synthetic field, `_kw`: the
    /// node's searchable token stream, identity tokens on a plaintext store
    /// and keyed HMAC digests on a sealed one. One field in BOTH states =
    /// one code path, and BM25 statistics (term frequencies, doc lengths)
    /// are identical by construction. Re-registering rebuilds the index —
    /// the tepin analog of `ensure_fts`.
    pub(super) fn ensure_keyword_fields(&self) -> Result<()> {
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

    // ---- at-rest sealing ----------------------------------------

    /// Read this store's recorded state from its own meta and load the key
    /// when the state needs one. Called at open; never at `open_in_memory`
    /// (tests must not touch the keyring).
    pub(super) fn load_encryption(&self) -> Result<()> {
        let state = Store::kv_get(self, "encryption_state")?
            .as_deref()
            .and_then(EncryptionState::parse)
            .unwrap_or(EncryptionState::Plaintext);
        *self.enc.writing() = state;
        if state != EncryptionState::Plaintext && self.seal_key().is_none() {
            *self.key.writing() = SealKey::load_or_create().map(Arc::new);
        }
        Ok(())
    }

    pub(super) fn enc_state(&self) -> EncryptionState {
        *self.enc.reading()
    }

    pub(super) fn seal_key(&self) -> Option<Arc<SealKey>> {
        self.key.reading().clone()
    }

    /// Inject a key without touching the keyring — tests only.
    pub fn set_seal_key_for_tests(&self, key: SealKey) {
        *self.key.writing() = Some(Arc::new(key));
    }

    /// Seal one string for storage under the current state (no-op on a
    /// plaintext store or without a key — never corrupt).
    pub(super) fn seal_write(&self, s: &str) -> String {
        match (self.enc_state().writes_sealed(), self.seal_key()) {
            (true, Some(k)) => k.seal(s),
            _ => s.to_string(),
        }
    }

    /// Open one stored string for a reader: plaintext passes through, a
    /// sealed blob decrypts, and a blob we can't open renders the
    /// placeholder — never garbage, never an error.
    pub(super) fn unseal_read(&self, s: &str) -> String {
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
    pub(super) fn kw_field(&self, n: &Node) -> String {
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
    pub(super) fn kw_transform_tokens(&self, tokens: Vec<String>) -> String {
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
    pub(super) fn doc_for_node(&self, n: &Node) -> Result<Value> {
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
    pub(super) fn node_from_doc(&self, mut doc: Value, policy: &PolicyConfig) -> Result<Node> {
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
    pub(super) fn doc_for_edge(&self, e: &Edge) -> Result<Value> {
        let mut doc = serde_json::to_value(e)?;
        doc["_id"] = json!(e.id);
        if let Some(note) = &e.note
            && self.enc_state().writes_sealed()
        {
            doc["note"] = json!(self.seal_write(note));
        }
        Ok(doc)
    }

    pub(super) fn edge_from_doc(&self, mut doc: Value) -> Result<Edge> {
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
    pub(super) fn audit_doc_recode(&self, mut doc: Value) -> Result<Value> {
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
    pub(super) fn audit_from_doc(&self, mut doc: Value) -> Result<AuditEntry> {
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

    /// Task → stored document: title and body seal like a node's when the
    /// state seals writes.
    pub(super) fn doc_for_task(&self, t: &crate::tasks::Task) -> Result<Value> {
        let mut doc = serde_json::to_value(t)?;
        if self.enc_state().writes_sealed() && self.seal_key().is_some() {
            doc["title"] = json!(self.seal_write(&t.title));
            if let Some(b) = &t.body {
                doc["body"] = json!(self.seal_write(b));
            }
        }
        doc["_id"] = json!(t.id);
        Ok(doc)
    }

    pub(super) fn task_from_doc(&self, mut doc: Value) -> Result<crate::tasks::Task> {
        for member in ["title", "body"] {
            if let Some(s) = doc[member].as_str()
                && is_sealed(s)
            {
                let open = self.unseal_read(s);
                doc[member] = json!(open);
            }
        }
        Ok(serde_json::from_value(doc)?)
    }

    /// Nodes matching an equality filter on an indexed field (type,
    /// session_id), hydrated like `all_nodes`, oldest first. Only the
    /// matching documents are read and unsealed.
    pub(super) fn nodes_matching(&self, filter: Value) -> Result<Vec<Node>> {
        let policy = self.policy();
        let mut out: Vec<Node> = self
            .find_docs(NODES, &filter)?
            .into_iter()
            .map(|d| self.node_from_doc(d, &policy))
            .collect::<Result<_>>()?;
        out.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        Ok(out)
    }

    pub(super) fn edges_matching(&self, field: &str, id: &str) -> Result<Vec<Edge>> {
        self.find_docs(EDGES, &json!({ field: id }))?
            .into_iter()
            .map(|d| self.edge_from_doc(d))
            .collect()
    }

    pub(super) fn suspects_matching(&self, filter: Value) -> Result<Vec<Suspect>> {
        self.find_docs(SUSPECTS, &filter)?
            .into_iter()
            .map(doc_suspect)
            .collect()
    }

    /// The `model_id` stamped on stored vectors — the recorded identity, or
    /// the default model for stores that predate model selection.
    pub(super) fn vector_model_id(&self) -> Result<String> {
        Ok(Store::embed_model(self)?.map_or_else(|| DEFAULT_EMBED_MODEL.to_string(), |m| m.name))
    }
}
