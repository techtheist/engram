//! The mutation journal: attribution, audit rows, and undo.

use super::*;

impl Engine {
    /// Stamp who the following writes belong to. Front-ends sharing this
    /// engine call it under their mutex lock before every operation.
    pub fn set_audit_origin(&mut self, origin: AuditOrigin) {
        self.audit_origin = origin;
    }

    /// Journal a session activity event (`mcp_session_started`, `brief_served`,
    /// …). `entity_id` is the session, so `audit(entity_id = session)` pages one
    /// session's arc.
    pub fn audit_activity(&self, action: &str, note: Option<String>) -> Result<()> {
        let session = self.audit_origin.session_id.clone().unwrap_or_default();
        self.audit(action, "session", &session, note, None, None, None)
    }

    /// The acting session, as audit rows stamp it.
    pub(crate) fn audit_session(&self) -> Option<String> {
        self.audit_origin
            .session_id
            .clone()
            .filter(|s| !s.is_empty())
    }

    /// The acting front-end: pane | mcp | daemon | cli | library.
    pub(crate) fn audit_origin_name(&self) -> String {
        self.audit_origin.origin.clone()
    }

    /// Journal one row for a sibling module (tasks), under this engine's
    /// origin and session.
    pub(crate) fn audit_row(
        &self,
        action: &str,
        entity: &str,
        entity_id: &str,
        title: Option<String>,
        before: Option<serde_json::Value>,
        after: Option<serde_json::Value>,
    ) -> Result<()> {
        self.audit(action, entity, entity_id, title, before, after, None)
    }

    /// One page of the journal, newest first (keyset pagination on `seq`).
    /// A row an undo reverted carries `undone_by`, unless that undo was
    /// itself undone.
    pub fn audit_log(
        &self,
        before: Option<i64>,
        entity_id: Option<&str>,
        limit: usize,
    ) -> Result<AuditPage> {
        let mut page = self.store.audit_page(before, entity_id, limit)?;
        let (undone, _) = self.undo_state()?;
        for e in &mut page.entries {
            e.undone_by = undone.get(&e.seq).copied();
        }
        Ok(page)
    }

    /// What the journal says about undos: original seq → the
    /// `undone` row that reverted it (only undos that still stand), and every
    /// operation an undo wrote.
    fn undo_state(
        &self,
    ) -> Result<(
        std::collections::HashMap<i64, i64>,
        std::collections::HashSet<String>,
    )> {
        let marks = self.store.audit_rows(&AuditQuery {
            action: Some(UNDONE.into()),
            ..AuditQuery::default()
        })?;
        let target = |m: &AuditEntry| m.entity_id.parse::<i64>().ok();
        let targets: std::collections::HashSet<i64> = marks.iter().filter_map(target).collect();
        let undo_ops: std::collections::HashSet<String> =
            marks.iter().filter_map(|m| m.op_id.clone()).collect();
        // An undo stands until one of its own writes is undone.
        let mut reverted = std::collections::HashSet::new();
        for op in &undo_ops {
            let rows = self.store.audit_rows(&AuditQuery {
                op_id: Some(op.clone()),
                ..AuditQuery::default()
            })?;
            if rows
                .iter()
                .any(|r| r.action != UNDONE && targets.contains(&r.seq))
            {
                reverted.insert(op.clone());
            }
        }
        let mut undone = std::collections::HashMap::new();
        for m in &marks {
            if m.op_id.as_ref().is_some_and(|op| reverted.contains(op)) {
                continue;
            }
            if let Some(t) = target(m) {
                undone.insert(t, m.seq);
            }
        }
        Ok((undone, undo_ops))
    }

    /// Undo one journal row, or — when it belongs to an operation (a merge, a
    /// delete with its cascade, an earlier undo) — the whole operation
    ///. User-only, like hard delete: served to the pane over HTTP,
    /// never to MCP. `dry_run` reports what would happen and changes nothing.
    pub fn undo_entry(&self, seq: i64, dry_run: bool) -> Result<UndoReport> {
        let Some(row) = self
            .store
            .audit_rows(&AuditQuery {
                seq: Some(seq),
                ..AuditQuery::default()
            })?
            .pop()
        else {
            return Err(crate::Error::NotFound(format!("audit row #{seq}")));
        };
        let rows = match &row.op_id {
            Some(op) => self.store.audit_rows(&AuditQuery {
                op_id: Some(op.clone()),
                ..AuditQuery::default()
            })?,
            None => vec![row],
        };
        self.undo_rows(rows, dry_run)
    }

    /// Undo everything one session wrote, newest first: the notes it
    /// created are removed, its edits and verdicts are reverted. Rows that
    /// changed afterwards in another session are skipped and reported.
    pub fn undo_session(&self, session_id: &str, dry_run: bool) -> Result<UndoReport> {
        let rows = self.store.audit_rows(&AuditQuery {
            session_id: Some(session_id.to_string()),
            ..AuditQuery::default()
        })?;
        if !rows.iter().any(undoable) {
            return Err(crate::Error::NotFound(format!(
                "session {session_id} wrote nothing that can be undone"
            )));
        }
        self.undo_rows(rows, dry_run)
    }

    fn undo_rows(&self, rows: Vec<AuditEntry>, dry_run: bool) -> Result<UndoReport> {
        let _op = self.op_scope();
        let this_op = self.audit_op.locked().clone();
        let (undone, undo_ops) = self.undo_state()?;
        let mut rows: Vec<AuditEntry> = rows.into_iter().filter(undoable).collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.seq));
        let batch: std::collections::HashSet<i64> = rows.iter().map(|r| r.seq).collect();
        let mut report = UndoReport {
            dry_run,
            ..UndoReport::default()
        };
        // What a dry run has "done" so far, so later rows see it: id → exists.
        let mut sim: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        for row in rows {
            let item = |note: Option<String>| UndoItem {
                seq: row.seq,
                action: row.action.clone(),
                entity: row.entity.clone(),
                entity_id: row.entity_id.clone(),
                title: row.title.clone(),
                effect: undo_effect(&row).to_string(),
                note,
            };
            if let Some(by) = undone.get(&row.seq) {
                report
                    .skipped
                    .push(item(Some(format!("already undone by #{by}"))));
                continue;
            }
            // Changed since: a later write to the same entity that is not part
            // of this undo, not itself undone, and not written by an undo.
            let later = self.store.audit_rows(&AuditQuery {
                entity_id: Some(row.entity_id.clone()),
                after: Some(row.seq),
                ..AuditQuery::default()
            })?;
            let newer = later.iter().find(|r| {
                undoable(r)
                    && !batch.contains(&r.seq)
                    && !undone.contains_key(&r.seq)
                    && !r
                        .op_id
                        .as_ref()
                        .is_some_and(|op| undo_ops.contains(op) || this_op.as_ref() == Some(op))
            });
            if let Some(newer) = newer {
                report.skipped.push(item(Some(format!(
                    "changed since: #{} ({}) is newer — undo it first",
                    newer.seq, newer.action
                ))));
                continue;
            }
            match self.undo_one(&row, dry_run, &mut sim)? {
                Ok(note) => {
                    if !dry_run {
                        self.audit(
                            UNDONE,
                            "audit",
                            &row.seq.to_string(),
                            Some(format!(
                                "undid #{} — {} {} {}",
                                row.seq,
                                row.action,
                                row.entity,
                                row.title.as_deref().unwrap_or(&row.entity_id)
                            )),
                            None,
                            None,
                            None,
                        )?;
                    }
                    report.undone.push(item(note));
                }
                Err(reason) => report.skipped.push(item(Some(reason))),
            }
        }
        Ok(report)
    }

    /// Revert one row. The outer `Err` is a store failure; the inner one is
    /// the reason this row cannot be undone. `Ok(Some(note))` reports what
    /// else the undo touched.
    fn undo_one(
        &self,
        row: &AuditEntry,
        dry_run: bool,
        sim: &mut std::collections::HashMap<String, bool>,
    ) -> Result<std::result::Result<Option<String>, String>> {
        let node_exists = |sim: &std::collections::HashMap<String, bool>, id: &str| {
            Ok::<bool, crate::Error>(match sim.get(id) {
                Some(present) => *present,
                None => self.store.get_node(id)?.is_some(),
            })
        };
        let edge_exists = |sim: &std::collections::HashMap<String, bool>, id: &str| {
            Ok::<bool, crate::Error>(match sim.get(id) {
                Some(present) => *present,
                None => self.store.get_edge(id)?.is_some(),
            })
        };
        let id = row.entity_id.as_str();
        match (row.entity.as_str(), &row.before, &row.after) {
            // A created note: remove it, and the links it gathered.
            ("node", None, Some(_)) => {
                if !node_exists(sim, id)? {
                    return Ok(Err("the note is already gone".into()));
                }
                let links = self.store.edges_out(id)?.len() + self.store.edges_in(id)?.len();
                if !dry_run {
                    self.delete_node(id)?;
                }
                sim.insert(id.to_string(), false);
                Ok(Ok((links > 0).then(|| {
                    format!(
                        "also removes {links} link{}",
                        if links == 1 { "" } else { "s" }
                    )
                })))
            }
            // A deleted note: bring it back as it was.
            ("node", Some(before), None) => {
                if node_exists(sim, id)? {
                    return Ok(Err("a note with this id exists again".into()));
                }
                let node: Node = serde_json::from_value(before.clone())?;
                if !dry_run {
                    self.restore_node(&node, None)?;
                }
                sim.insert(id.to_string(), true);
                Ok(Ok(None))
            }
            // An edit, verdict or trust change: put the note back as it was.
            ("node", Some(before), Some(_)) => {
                if !node_exists(sim, id)? {
                    return Ok(Err("the note is gone".into()));
                }
                let node: Node = serde_json::from_value(before.clone())?;
                if !dry_run {
                    let current = self.store.get_node(id)?;
                    self.restore_node(&node, current.as_ref())?;
                }
                Ok(Ok(None))
            }
            ("edge", None, Some(_)) => {
                if !edge_exists(sim, id)? {
                    return Ok(Err("the link is already gone".into()));
                }
                if !dry_run {
                    self.delete_edge(id)?;
                }
                sim.insert(id.to_string(), false);
                Ok(Ok(None))
            }
            ("edge", Some(before), after) => {
                let edge: Edge = serde_json::from_value(before.clone())?;
                let exists = edge_exists(sim, id)?;
                match after {
                    None if exists => return Ok(Err("the link exists again".into())),
                    Some(_) if !exists => return Ok(Err("the link is gone".into())),
                    _ => {}
                }
                if !node_exists(sim, &edge.from_id)? || !node_exists(sim, &edge.to_id)? {
                    return Ok(Err("one of its notes is gone".into()));
                }
                if !dry_run {
                    let current = self.store.get_edge(id)?;
                    self.restore_edge(&edge, current.as_ref())?;
                }
                sim.insert(id.to_string(), true);
                Ok(Ok(None))
            }
            _ => Ok(Err("this row cannot be undone".into())),
        }
    }

    /// Put a note back exactly as a journal snapshot recorded it, re-embedded.
    fn restore_node(&self, node: &Node, current: Option<&Node>) -> Result<()> {
        self.store.upsert_node(node)?;
        self.embed_node(node)?;
        let stored = self
            .store
            .get_node(&node.id)?
            .unwrap_or_else(|| node.clone());
        self.audit_node(RESTORED, current, Some(&stored))?;
        self.notify(match current {
            Some(_) => ChangeEvent::NodeUpdated(stored),
            None => ChangeEvent::NodeAdded(stored),
        });
        Ok(())
    }

    /// Put a link back exactly as a journal snapshot recorded it. A restored
    /// `conflicts-with` demotes again, a restored earlier form lifts it.
    fn restore_edge(&self, edge: &Edge, current: Option<&Edge>) -> Result<()> {
        self.store.upsert_edge(edge)?;
        self.audit_edge(RESTORED, current, Some(edge))?;
        self.notify(match current {
            Some(_) => ChangeEvent::EdgeUpdated(edge.clone()),
            None => ChangeEvent::EdgeAdded(edge.clone()),
        });
        self.reconcile_conflict_demotion(edge)?;
        Ok(())
    }

    /// Mark the journal rows written from here until the scope drops as one
    /// operation. Nested scopes join the outer one.
    pub(super) fn op_scope(&self) -> OpScope<'_> {
        let mut op = self.audit_op.locked();
        if op.is_some() {
            return OpScope { slot: None };
        }
        *op = Some(crate::id::new_id());
        OpScope {
            slot: Some(&self.audit_op),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn audit(
        &self,
        action: &str,
        entity: &str,
        entity_id: &str,
        title: Option<String>,
        before: Option<serde_json::Value>,
        after: Option<serde_json::Value>,
        session_id: Option<String>,
    ) -> Result<()> {
        self.store.add_audit(&AuditEntry {
            seq: 0,
            ts: crate::store::now(),
            action: action.to_string(),
            entity: entity.to_string(),
            entity_id: entity_id.to_string(),
            title,
            before,
            after,
            origin: self.audit_origin.origin.clone(),
            session_id: session_id.or_else(|| self.audit_origin.session_id.clone()),
            cwd: self.audit_cwd.clone(),
            pid: Some(self.audit_pid),
            version: Some(self.audit_version.clone()),
            op_id: self.audit_op.locked().clone(),
            undone_by: None,
        })
    }

    pub(super) fn audit_node(
        &self,
        action: &str,
        before: Option<&Node>,
        after: Option<&Node>,
    ) -> Result<()> {
        let Some(subject) = after.or(before) else {
            return Ok(());
        };
        // The node's stored session_id names its creator, so it only
        // attributes "created" rows; every later action is whoever holds the
        // engine now (the audit origin), not the session that made the node.
        let actor_session = match action {
            "created" => subject.session_id.clone(),
            _ => None,
        };
        self.audit(
            action,
            "node",
            &subject.id,
            Some(subject.title.clone()),
            before.map(serde_json::to_value).transpose()?,
            after.map(serde_json::to_value).transpose()?,
            actor_session,
        )
    }

    pub(super) fn audit_edge(
        &self,
        action: &str,
        before: Option<&Edge>,
        after: Option<&Edge>,
    ) -> Result<()> {
        let Some(subject) = after.or(before) else {
            return Ok(());
        };
        self.audit(
            action,
            "edge",
            &subject.id,
            Some(self.edge_label(subject)),
            before.map(serde_json::to_value).transpose()?,
            after.map(serde_json::to_value).transpose()?,
            None,
        )
    }

    /// Sentence-shaped display label for an edge's journal rows — endpoint
    /// titles are snapshotted so the row stays readable after deletions.
    fn edge_label(&self, e: &Edge) -> String {
        let title = |id: &str| {
            self.store
                .get_node(id)
                .ok()
                .flatten()
                .map_or_else(|| id.to_string(), |n| n.title)
        };
        format!(
            "\"{}\" {} \"{}\"",
            title(&e.from_id),
            e.edge_type.as_str(),
            title(&e.to_id)
        )
    }
}

/// The journal action of an undo's marker row (entity `audit`, entity_id =
/// the undone row's seq).
const UNDONE: &str = "undone";
/// The journal action of a note or link put back from a snapshot.
const RESTORED: &str = "restored";

/// Can this journal row be undone? Node and edge mutations with a snapshot;
/// never session activity, imports or undo markers.
fn undoable(r: &AuditEntry) -> bool {
    matches!(r.entity.as_str(), "node" | "edge")
        && r.action != "imported"
        && (r.before.is_some() || r.after.is_some())
}

/// What undoing a row means, in the pane's words.
fn undo_effect(r: &AuditEntry) -> &'static str {
    match (r.entity.as_str(), r.before.is_some(), r.after.is_some()) {
        ("node", false, true) => "removes the note",
        ("node", true, false) => "brings the note back",
        ("node", true, true) => "restores the note as it was",
        ("edge", false, true) => "removes the link",
        ("edge", true, false) => "brings the link back",
        ("edge", true, true) => "restores the link as it was",
        _ => "nothing",
    }
}

/// Clears the engine's current operation id when the operation that set it
/// ends (see [`Engine::op_scope`]).
pub(super) struct OpScope<'a> {
    pub(super) slot: Option<&'a std::sync::Mutex<Option<String>>>,
}

impl Drop for OpScope<'_> {
    fn drop(&mut self) {
        if let Some(slot) = self.slot {
            *slot.locked() = None;
        }
    }
}
