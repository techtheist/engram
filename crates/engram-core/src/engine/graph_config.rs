//! The per-graph configuration: ontology, presets, renames.

use super::*;

impl Engine {
    /// The stored per-graph configuration, or `None` when the graph runs on
    /// defaults. A corrupt document reads as `None` (defaults) — config must
    /// never be able to brick a store open.
    pub(super) fn stored_graph_config(&self) -> Option<crate::config::GraphConfig> {
        // Corrupt documents already warn at open (GraphConfig::from_stored);
        // here only the stored-vs-defaults distinction matters (exports).
        self.store
            .graph_config()
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
    }

    /// The live configuration this graph runs on: the store's
    /// cached parse, shared — the cheap accessor every read path should use.
    pub fn config(&self) -> std::sync::Arc<crate::config::GraphConfig> {
        self.store.config()
    }

    /// Owned clone of the live configuration — only for callers that
    /// serialize or mutate it (GET /config, the rename ops).
    pub fn graph_config(&self) -> crate::config::GraphConfig {
        (*self.store.config()).clone()
    }

    /// Resolve a caller's optional brief budget: explicit wins, otherwise
    /// the graph's configured `brief.total_chars` — the one rule every
    /// surface (HTTP, MCP, CLI) shares.
    pub fn brief_chars(&self, requested: Option<usize>) -> usize {
        requested.unwrap_or_else(|| self.store.config().brief.total_chars)
    }

    /// The ontology's default durability for a node type when the caller
    /// doesn't specify one (each TypeDef carries its default; unknown types
    /// are caught by the write-boundary check, episodic here is moot).
    pub fn default_durability(&self, t: &NodeType) -> Durability {
        self.store
            .config()
            .type_def(t.as_str())
            .map_or(Durability::Episodic, |d| d.durability)
    }

    /// Replace the graph's configuration — validated against the hard
    /// invariants first; a violation is a 400, never a partial write. A
    /// config change is a user gesture (pane/HTTP only) and journals like
    /// any other mutation.
    pub fn set_graph_config(&self, cfg: &crate::config::GraphConfig) -> Result<()> {
        cfg.validate()?;
        // In-use guard: a PUT can't strand stored knowledge. Dropping a type
        // that still has nodes (or a verb that still has edges) is refused —
        // rename (bulk retype) or retype first; renames must go through
        // `rename_type`/`rename_verb`, which move the stored rows along.
        let current = self.store.config();
        let dropped_types: Vec<&str> = current
            .ontology
            .types
            .iter()
            .filter(|t| cfg.type_def(&t.name).is_none())
            .map(|t| t.name.as_str())
            .collect();
        if !dropped_types.is_empty() {
            let mut counts: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            let nodes = self.store.all_nodes()?;
            for node in &nodes {
                if let Some(name) = dropped_types
                    .iter()
                    .find(|t| **t == node.node_type.as_str())
                {
                    *counts.entry(name).or_default() += 1;
                }
            }
            if let Some((name, n)) = counts.into_iter().next() {
                return Err(crate::Error::Config(format!(
                    "type {name:?} still has {n} node(s) — rename it (bulk retype) or retype them first"
                )));
            }
        }
        let dropped_verbs: Vec<&str> = current
            .ontology
            .verbs
            .iter()
            .filter(|v| cfg.verb_def(&v.name).is_none())
            .map(|v| v.name.as_str())
            .collect();
        if !dropped_verbs.is_empty() {
            let mut counts: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            let edges = self.store.all_edges()?;
            for edge in &edges {
                if let Some(name) = dropped_verbs
                    .iter()
                    .find(|v| **v == edge.edge_type.as_str())
                {
                    *counts.entry(name).or_default() += 1;
                }
            }
            if let Some((name, n)) = counts.into_iter().next() {
                return Err(crate::Error::Config(format!(
                    "verb {name:?} still has {n} edge(s) — rename it or retype them first"
                )));
            }
        }
        let before = self.stored_graph_config().map(|c| serde_json::json!(c));
        self.store.set_graph_config(&serde_json::to_string(cfg)?)?;
        self.audit(
            "config_updated",
            "graph",
            "",
            Some(format!(
                "{} types / {} verbs, preset {}",
                cfg.ontology.types.len(),
                cfg.ontology.verbs.len(),
                cfg.ontology.preset
            )),
            before,
            Some(serde_json::json!(cfg)),
            None,
        )?;
        self.notify(ChangeEvent::ConfigChanged);
        self.sync_history_store();
        // A changed indexed-field set rewrites + re-embeds affected storage
        // (cheap no-op when the fingerprint is unchanged).
        self.ensure_field_index()?;
        Ok(())
    }

    /// Apply a shipped preset's ontology only; policy (with auto-tuned lines),
    /// brief, versioning, history and field definitions are kept (Caution
    /// 00e3547lopga). Same guards and audit row as [`Engine::set_graph_config`].
    pub fn apply_preset(&self, id: &str) -> Result<crate::config::GraphConfig> {
        let preset = crate::config::presets()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| {
                let known: Vec<String> =
                    crate::config::presets().into_iter().map(|p| p.id).collect();
                crate::Error::Config(format!(
                    "unknown preset {id:?} — known: {}",
                    known.join(", ")
                ))
            })?;
        let mut cfg = self.graph_config();
        cfg.ontology = preset.config.ontology;
        self.set_graph_config(&cfg)?;
        Ok(cfg)
    }

    /// Rewrite and re-embed every node when the indexed custom-field set changes
    /// (tracked by a fingerprint in store meta); returns the count. Skipped under a
    /// fake embedder, leaving the fingerprint for the next real open.
    pub fn ensure_field_index(&self) -> Result<usize> {
        const FP_KEY: &str = "indexed_fields_fp";
        let fp = self.store.config().indexed_field_names().join(",");
        if self.store.kv_get(FP_KEY)?.unwrap_or_default() == fp {
            return Ok(0);
        }
        let nodes = self.store.all_nodes()?;
        if self.embedder.is_fake() && !nodes.is_empty() {
            return Ok(0);
        }
        for n in &nodes {
            self.store.upsert_node(n)?;
            self.embed_node(n)?;
        }
        self.store.kv_set(FP_KEY, &fp)?;
        Ok(nodes.len())
    }

    /// Rename a custom field AND move every stored value with it — the
    /// field-migration gesture, mirroring [`Engine::rename_type`]. Returns
    /// how many nodes carried the field.
    pub fn rename_field(&self, from: &str, to: &str) -> Result<u64> {
        if from == to {
            return Err(crate::Error::Config("rename needs a new name".into()));
        }
        let mut cfg = (*self.store.config()).clone();
        let def = cfg
            .fields
            .iter_mut()
            .find(|f| f.name == from)
            .ok_or_else(|| crate::Error::Config(format!("unknown field {from:?}")))?;
        def.name = to.to_string();
        cfg.validate()?;
        // Move the values first (idempotent on re-run), then persist the
        // config — same ordering rationale as rename_type.
        let mut moved = 0u64;
        for node in self.store.all_nodes()? {
            let Some(fields) = &node.fields else { continue };
            if !fields.contains_key(from) {
                continue;
            }
            let mut next = fields.clone();
            if let Some(v) = next.remove(from) {
                next.insert(to.to_string(), v);
            }
            let mut node = node;
            node.fields = Some(next);
            self.store.upsert_node(&node)?;
            moved += 1;
        }
        self.set_graph_config(&cfg)?;
        self.audit(
            "field_renamed",
            "graph",
            "",
            Some(format!("{from} → {to} ({moved} nodes carried the field)")),
            None,
            None,
            None,
        )?;
        Ok(moved)
    }

    /// Rename a node type AND bulk-retype every stored node of it — the
    /// ontology-migration gesture. Roles, hue, brief section and
    /// durability ride along unchanged; only the name moves. Returns how
    /// many nodes followed.
    pub fn rename_type(&self, from: &str, to: &str) -> Result<u64> {
        if from == to {
            return Err(crate::Error::Config("rename needs a new name".into()));
        }
        let mut cfg = (*self.store.config()).clone();
        let def = cfg
            .ontology
            .types
            .iter_mut()
            .find(|t| t.name == from)
            .ok_or_else(|| crate::Error::Config(format!("unknown type {from:?}")))?;
        def.name = to.to_string();
        cfg.validate()?;
        // Retype first, then persist: if the config write fails the retype
        // is legal either way (a re-run is idempotent), while the reverse
        // order could strand rows under a name the config no longer knows.
        let renamed = self.store.retype_nodes(from, to)?;
        self.set_graph_config(&cfg)?;
        self.audit(
            "type_renamed",
            "graph",
            "",
            Some(format!("{from} → {to} ({renamed} nodes retyped)")),
            None,
            None,
            None,
        )?;
        Ok(renamed)
    }

    /// Rename an edge verb AND bulk-retype every stored edge of it. Role
    /// flags (supersession/contradiction/…) ride along unchanged.
    pub fn rename_verb(&self, from: &str, to: &str) -> Result<u64> {
        if from == to {
            return Err(crate::Error::Config("rename needs a new name".into()));
        }
        let mut cfg = (*self.store.config()).clone();
        let def = cfg
            .ontology
            .verbs
            .iter_mut()
            .find(|v| v.name == from)
            .ok_or_else(|| crate::Error::Config(format!("unknown verb {from:?}")))?;
        def.name = to.to_string();
        cfg.validate()?;
        let renamed = self.store.retype_edges(from, to)?;
        self.set_graph_config(&cfg)?;
        self.audit(
            "verb_renamed",
            "graph",
            "",
            Some(format!("{from} → {to} ({renamed} edges retyped)")),
            None,
            None,
            None,
        )?;
        Ok(renamed)
    }
}
