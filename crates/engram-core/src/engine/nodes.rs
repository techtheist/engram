//! Node writes: create, update, trust acts, delete and tombstones, plus input validation.

use super::*;

impl Engine {
    /// Add a node and embed it (full-field composition) in one step. Trust is computed
    /// from timestamps at read time; user-authored nodes are approved by
    /// construction (the store stamps `approved_at`).
    pub fn add_node(&self, n: NewNode) -> Result<Node> {
        self.add_node_opts(n, true, None)
    }

    /// `enforce_required_fields: false` is reserved for engine-minted notes
    /// (the tombstone a hard delete leaves): a required custom field is a
    /// contract for authors, and must never be able to veto a user's delete.
    /// Provided values are still validated either way.
    pub(super) fn add_node_opts(
        &self,
        mut n: NewNode,
        enforce_required_fields: bool,
        cache: Option<&EmbedCache>,
    ) -> Result<Node> {
        self.check_node_type(&n.node_type)?;
        let cfg = self.store.config();
        // Worklist-role types are live items from birth — the write boundary
        // owns the default so every surface (MCP, pane, raw HTTP) gets it.
        if n.status.is_none()
            && cfg
                .type_def(n.node_type.as_str())
                .is_some_and(|t| t.roles.worklist)
        {
            n.status = Some(NodeStatus::Open);
        }
        // Version tracking: auto-stamp the current working version on
        // version-bound types (explicit versions — digestion of historical
        // material — always win).
        if n.version.is_none()
            && cfg.versioning.enabled
            && cfg
                .type_def(n.node_type.as_str())
                .is_none_or(|t| t.roles.versioned)
        {
            n.version = self.store.current_version()?;
        }
        // Custom fields: an empty map is the same as none; the full
        // map is validated here so every surface gets one teaching error.
        if n.fields.as_ref().is_some_and(serde_json::Map::is_empty) {
            n.fields = None;
        }
        self.check_fields_ext(&n.node_type, n.fields.as_ref(), enforce_required_fields)?;
        let node = self.store.add_node(n)?;
        self.embed_node_with(&node, cache)?;
        self.audit_node("created", None, Some(&node))?;
        self.notify(ChangeEvent::NodeAdded(node.clone()));
        Ok(node)
    }

    /// The write boundary of ontology-as-data: a node type must
    /// exist in this graph's ontology. Shape was checked at parse; existence
    /// can only be checked here, where the graph's config is known.
    pub(super) fn check_node_type(&self, t: &NodeType) -> Result<()> {
        let cfg = self.store.config();
        if cfg.type_def(t.as_str()).is_none() {
            return Err(crate::Error::Config(format!(
                "unknown node type {:?} — this graph's ontology defines: {}",
                t.as_str(),
                cfg.ontology
                    .types
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        Ok(())
    }

    /// Validate the full resolved field map (unknown names, kinds, required
    /// fields). The error teaches: it names the graph's whole field roster and the
    /// call shape, so an agent can self-correct in one step.
    fn check_fields(
        &self,
        node_type: &NodeType,
        fields: Option<&serde_json::Map<String, serde_json::Value>>,
    ) -> Result<()> {
        self.check_fields_ext(node_type, fields, true)
    }

    fn check_fields_ext(
        &self,
        node_type: &NodeType,
        fields: Option<&serde_json::Map<String, serde_json::Value>>,
        enforce_required: bool,
    ) -> Result<()> {
        let cfg = self.store.config();
        let empty = serde_json::Map::new();
        let fields = fields.unwrap_or(&empty);
        if cfg.fields.is_empty() {
            if fields.is_empty() {
                return Ok(());
            }
            return Err(crate::Error::Config(
                "this graph declares no custom fields — drop the `fields` object \
                 (definitions are a user gesture: pane Settings or PUT /config)"
                    .into(),
            ));
        }
        let applicable = cfg.fields_for(node_type.as_str());
        let roster = || {
            let lines: Vec<String> = applicable
                .iter()
                .map(|f| {
                    format!(
                        "{} ({}{}{})",
                        f.name,
                        f.kind.as_str(),
                        if f.required { ", required" } else { "" },
                        if f.kind == crate::config::FieldKind::Enum {
                            format!(": {}", f.values.join(" | "))
                        } else {
                            String::new()
                        }
                    )
                })
                .collect();
            if lines.is_empty() {
                format!("no custom fields apply to {}", node_type.as_str())
            } else {
                format!(
                    "fields for {}: {} — pass as {{\"fields\": {{\"name\": value}}}}",
                    node_type.as_str(),
                    lines.join("; ")
                )
            }
        };
        for (name, value) in fields {
            let Some(def) = applicable.iter().find(|f| f.name == *name) else {
                return Err(crate::Error::Config(format!(
                    "unknown custom field {name:?} for type {} — {}",
                    node_type.as_str(),
                    roster()
                )));
            };
            if let Err(why) = field_value_ok(def, value) {
                return Err(crate::Error::Config(format!(
                    "custom field {name:?}: {why} — {}",
                    roster()
                )));
            }
        }
        let missing: Vec<&str> = applicable
            .iter()
            .filter(|f| enforce_required && f.required && !fields.contains_key(&f.name))
            .map(|f| f.name.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(crate::Error::Config(format!(
                "missing required custom field(s) {} for type {} — {}",
                missing.join(", "),
                node_type.as_str(),
                roster()
            )));
        }
        Ok(())
    }

    /// Resolve a patch's MERGE intent against the stored map: present keys
    /// overwrite, `null` deletes, absent keys survive. Returns the full
    /// replacement map the store gets (empty = clear all).
    pub(super) fn resolve_fields_patch(
        before: Option<&serde_json::Map<String, serde_json::Value>>,
        patch: &serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut out = before.cloned().unwrap_or_default();
        for (k, v) in patch {
            if v.is_null() {
                out.remove(k);
            } else {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }

    /// The bitemporal-search boundary: a `date_field` clock must
    /// name declared date-kind custom fields, and must travel with a window
    /// — an unaimed clock filters nothing and would silently return the
    /// default result while looking like an event-time answer.
    pub(super) fn check_clock(&self, filter: &SearchFilter) -> Result<()> {
        use crate::timespec::TimeClock;
        let names: Vec<&str> = match &filter.clock {
            TimeClock::CreatedAt => return Ok(()),
            TimeClock::Field(name) => vec![name.as_str()],
            TimeClock::FieldSpan(from, to) => vec![from.as_str(), to.as_str()],
        };
        let cfg = self.store.config();
        for name in names {
            let ok = cfg
                .field_def(name)
                .is_some_and(|f| f.kind == crate::config::FieldKind::Date);
            if !ok {
                let dates: Vec<&str> = cfg
                    .fields
                    .iter()
                    .filter(|f| f.kind == crate::config::FieldKind::Date)
                    .map(|f| f.name.as_str())
                    .collect();
                return Err(crate::Error::Config(format!(
                    "date_field {name:?} is not a declared date-kind custom field — {}",
                    if dates.is_empty() {
                        "this graph declares none (definitions: pane Settings / PUT /config)"
                            .to_string()
                    } else {
                        format!("this graph's date fields: {}", dates.join(", "))
                    }
                )));
            }
        }
        if filter.window.is_open() {
            return Err(crate::Error::Config(
                "date_field needs a window — pass after and/or before to aim it".into(),
            ));
        }
        Ok(())
    }

    /// Same boundary for edge verbs: a triple can only use a verb this
    /// graph's ontology declares.
    pub(super) fn check_edge_type(&self, t: &EdgeType) -> Result<()> {
        let cfg = self.store.config();
        if cfg.verb_def(t.as_str()).is_none() {
            return Err(crate::Error::Config(format!(
                "unknown edge verb {:?} — this graph's ontology defines: {}",
                t.as_str(),
                cfg.ontology
                    .verbs
                    .iter()
                    .map(|v| v.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        Ok(())
    }

    /// Patch a node and re-embed if any embedded field changed (title, body,
    /// tags, code_refs). Any update refreshes `last_seen` (the store stamps
    /// it): edited knowledge is in-use knowledge.
    pub fn update_node(&self, id: &str, patch: NodePatch) -> Result<Node> {
        self.update_node_with(id, patch, None)
    }

    /// [`Engine::update_node`] taking vectors computed before the lock
    /// wherever their texts match.
    pub(super) fn update_node_with(
        &self,
        id: &str,
        mut patch: NodePatch,
        cache: Option<&EmbedCache>,
    ) -> Result<Node> {
        if let Some(t) = &patch.node_type {
            self.check_node_type(t)?;
        }
        let touches_text = patch.title.is_some()
            || patch.body.is_some()
            || patch.tags.is_some()
            || patch.code_refs.is_some()
            || patch.fields.is_some();
        let before = self.store.get_node(id)?;
        // Fields patch is merge intent (null deletes): resolve against the stored
        // node and validate the result, against the new type on a retype. A retype
        // alone re-validates too.
        if patch.fields.is_some() || patch.node_type.is_some() {
            let stored = before.as_ref().and_then(|b| b.fields.as_ref());
            let resolved = match &patch.fields {
                Some(p) => Self::resolve_fields_patch(stored, p),
                None => stored.cloned().unwrap_or_default(),
            };
            let target_type = patch
                .node_type
                .clone()
                .or_else(|| before.as_ref().map(|b| b.node_type.clone()));
            if let Some(t) = &target_type {
                self.check_fields(t, Some(&resolved))?;
            }
            if patch.fields.is_some() {
                patch.fields = Some(resolved);
            }
        }
        let node = self.store.update_node(id, patch)?;
        if touches_text {
            self.embed_node_with(&node, cache)?;
        }
        // Setting valid_until is the supersede flow (replaces verdict), not an
        // edit — journal it under its real name.
        let action = match &before {
            Some(b) if b.valid_until.is_none() && node.valid_until.is_some() => "archived",
            _ => "updated",
        };
        self.audit_node(action, before.as_ref(), Some(&node))?;
        self.notify(ChangeEvent::NodeUpdated(node.clone()));
        Ok(node)
    }

    /// Confirm a node still true without changing its content: stamps
    /// `confirmed_at` (restarting trust on the confirmed curve) and clears
    /// any evidence demotion. A deliberate act — the pane's "Confirm still
    /// true" — unlike retrieval, which never refreshes trust.
    pub fn reconfirm(&self, id: &str) -> Result<Node> {
        self.update_node(id, NodePatch::default())
    }

    /// Explicit approval: trust restarts at its ceiling — and on stable
    /// knowledge holds there until contradicting evidence lands. User action
    /// in the pane, or the assistant **only on explicit user demand /
    /// verbatim verification** (enforced by skill policy).
    pub fn approve(&self, id: &str) -> Result<Node> {
        let before = self.store.get_node(id)?;
        let node = self.store.approve(id)?;
        self.audit_node("approved", before.as_ref(), Some(&node))?;
        self.notify(ChangeEvent::NodeUpdated(node.clone()));
        Ok(node)
    }

    /// Withdraw an approval (and any pin): trust falls back to the
    /// confirmed/created anchor. User-only, like the endorsements it undoes.
    pub fn revoke_approval(&self, id: &str) -> Result<Node> {
        let before = self.store.get_node(id)?;
        let node = self.store.revoke_approval(id)?;
        // Journal what was actually withdrawn — the pane offers this action
        // on pinned-but-never-approved nodes too.
        let action = match &before {
            Some(b) if b.approved_at.is_some() => "unapproved",
            Some(b) if b.trust_override.is_some() => "unpinned",
            _ => return Ok(node), // nothing to withdraw — no-op, no row
        };
        self.audit_node(action, before.as_ref(), Some(&node))?;
        self.notify(ChangeEvent::NodeUpdated(node.clone()));
        Ok(node)
    }

    /// Set or clear the constant-trust pin. Pin = 1.0;
    /// any 0..=1 value is allowed; `None` unpins. Pinned nodes never decay,
    /// never auto-archive, and evidence events skip them — user-only, the
    /// durable-memory counterpart of hard delete.
    pub fn set_trust_override(&self, id: &str, value: Option<f64>) -> Result<Node> {
        let before = self.store.get_node(id)?;
        let node = self.store.set_trust_override(id, value)?;
        let action = if value.is_some() {
            "pinned"
        } else {
            "unpinned"
        };
        self.audit_node(action, before.as_ref(), Some(&node))?;
        self.notify(ChangeEvent::NodeUpdated(node.clone()));
        Ok(node)
    }

    pub fn delete_node(&self, id: &str) -> Result<bool> {
        let _op = self.op_scope();
        let before = self.store.get_node(id)?;
        // The store cascades the node's edges; journal them too, so
        // undoing the delete can bring the links back. They are journaled
        // first, so an undo — newest first — restores the node before them.
        let mut cascaded = self.store.edges_out(id)?;
        for e in self.store.edges_in(id)? {
            if !cascaded.iter().any(|c| c.id == e.id) {
                cascaded.push(e);
            }
        }
        let removed = self.store.delete_node(id)?;
        if removed {
            for e in &cascaded {
                self.audit_edge("deleted", Some(e), None)?;
            }
            self.audit_node("deleted", before.as_ref(), None)?;
            self.notify(ChangeEvent::NodeDeleted(id.to_string()));
        }
        Ok(removed)
    }

    /// User-only hard delete that leaves a tombstone-role note recording what was
    /// removed and why, so an agent can't re-learn it unnoticed. Without a
    /// tombstone type in the ontology it is a plain delete.
    ///
    /// `keep_text` carries the victim's body, tags and code_refs into the marker,
    /// so the `tombstoned` warning also fires on paraphrased content; off is the
    /// purge shape (identity and reason only). The marker takes over the victim's
    /// edges to anchor-role nodes; every other edge cascades with the victim.
    pub fn delete_node_with_tombstone(
        &self,
        id: &str,
        reason: Option<&str>,
        keep_text: bool,
    ) -> Result<(bool, Option<Node>)> {
        // One operation in the journal: undo reverts all of it.
        let _op = self.op_scope();
        let Some(victim) = self.store.get_node(id)? else {
            return Ok((false, None));
        };
        let cfg = self.store.config();
        let Some(ts_type) = cfg.tombstone_type().map(str::to_string) else {
            return Ok((self.delete_node(id)?, None));
        };
        let ts_def = cfg.type_def(&ts_type).expect("tombstone_type is declared");
        let mut body = format!(
            "Deleted {} {:?} (id {}).",
            victim.node_type.as_str(),
            victim.title,
            victim.id
        );
        match reason {
            Some(r) if !r.trim().is_empty() => {
                body.push_str("\n\n**Why:** ");
                body.push_str(r.trim());
            }
            _ => {}
        }
        let (tags, code_refs) = if keep_text {
            if let Some(text) = victim
                .body
                .as_deref()
                .map(str::trim)
                .filter(|b| !b.is_empty())
            {
                body.push_str("\n\n**Removed text:** ");
                body.push_str(text);
            }
            (victim.tags.clone(), victim.code_refs.clone())
        } else {
            (Vec::new(), Vec::new())
        };
        // The mint skips required-field enforcement: deletion isn't
        // authoring, and field config must never veto a user's delete.
        let tombstone = self.add_node_opts(
            NewNode {
                node_type: NodeType::parse(&ts_type)?,
                title: format!("Removed: {}", victim.title),
                body: Some(body),
                created_at: None,
                durability: ts_def.durability,
                source: Source::User,
                session_id: None,
                status: None,
                code_refs,
                tags,
                version: None,
                props: None,
                fields: None,
            },
            false,
            None,
        )?;
        // Rehome the victim's live anchor edges before the cascade takes
        // them. Same contract as merge's rehoming: the edge keeps its id and
        // timestamps — the connection moved, it didn't recur.
        for edge in self.store.edges_out(&victim.id)? {
            if edge.valid_until.is_some() {
                continue;
            }
            let Some(target) = self.store.get_node(&edge.to_id)? else {
                continue;
            };
            if !is_anchor(&cfg, &target) {
                continue;
            }
            let before = edge.clone();
            let mut moved = edge;
            moved.from_id = tombstone.id.clone();
            self.store.upsert_edge(&moved)?;
            self.audit_edge("updated", Some(&before), Some(&moved))?;
            self.notify(ChangeEvent::EdgeUpdated(moved));
        }
        let removed = self.delete_node(id)?;
        Ok((removed, Some(tombstone)))
    }

    pub fn get_node(&self, id: &str) -> Result<Option<Node>> {
        self.store.get_node(id)
    }
}

/// Does `value` satisfy a field definition's kind? Values are scalars only —
/// arrays/objects are refused for every kind, so redaction, hoisting, and
/// the pane can all treat them as displayable atoms.
pub(super) fn field_value_ok(
    def: &crate::config::FieldDef,
    value: &serde_json::Value,
) -> Result2<(), String> {
    use crate::config::FieldKind::*;
    use serde_json::Value;
    match def.kind {
        Text => match value {
            Value::String(_) => Ok(()),
            _ => Err("expected a string".into()),
        },
        Url => match value {
            Value::String(s) if s.contains("://") => Ok(()),
            Value::String(_) => Err("expected a URL (scheme://…)".into()),
            _ => Err("expected a URL string".into()),
        },
        Number => match value {
            Value::Number(_) => Ok(()),
            _ => Err("expected a number".into()),
        },
        Bool => match value {
            Value::Bool(_) => Ok(()),
            _ => Err("expected true or false".into()),
        },
        Date => match value {
            Value::Number(n) if n.is_i64() => Ok(()),
            Value::String(s)
                if s.len() >= 10
                    && s.as_bytes()[..4].iter().all(u8::is_ascii_digit)
                    && crate::timespec::parse_instant(s, crate::store::now()).is_some() =>
            {
                Ok(())
            }
            Value::String(_) => {
                Err("expected \"YYYY-MM-DD\", an ISO instant, or unix seconds".into())
            }
            _ => Err("expected a date string or unix seconds".into()),
        },
        Enum => match value {
            Value::String(s) if def.values.iter().any(|v| v == s) => Ok(()),
            Value::String(s) => Err(format!("{s:?} is not one of: {}", def.values.join(" | "))),
            _ => Err("expected one of the declared enum values (a string)".into()),
        },
    }
}

pub(super) fn is_tombstone(cfg: &crate::config::GraphConfig, n: &Node) -> bool {
    cfg.type_def(n.node_type.as_str())
        .is_some_and(|t| t.roles.tombstone)
}

/// The victim's title as a delete mint wrote it into the marker's title
/// ("Removed: <title>"); a marker written by hand comes back whole.
pub(super) fn tombstone_victim_title(title: &str) -> &str {
    title
        .trim()
        .strip_prefix("Removed:")
        .map_or(title.trim(), str::trim)
}

/// Titles compared as the same claim: case-folded alphanumeric words.
pub(super) fn normalized_title(t: &str) -> String {
    t.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The tombstone's account of the removal, for riding on a warning: the
/// body's opening (which the delete mint fills with the victim's identity
/// and the "**Why:**" line), word-cut so a carried-over victim body doesn't
/// turn the verdict into a transcript.
pub(super) fn tombstone_note(n: &Node) -> Option<String> {
    const TOMBSTONE_NOTE_CHARS: usize = 280;
    let body = n.body.as_deref()?.trim();
    if body.is_empty() {
        return None;
    }
    Some(excerpt_words(
        &body.replace('\n', " "),
        TOMBSTONE_NOTE_CHARS,
    ))
}
