//! Edge writes and what they set off: supersession, conflict demotion, inheritance.

use super::*;

impl Engine {
    pub fn add_edge(&self, e: NewEdge) -> Result<Edge> {
        self.check_edge_type(&e.edge_type)?;
        let edge = self.store.add_edge(e)?;
        self.audit_edge("created", None, Some(&edge))?;
        self.notify(ChangeEvent::EdgeAdded(edge.clone()));
        self.reconcile_conflict_demotion(&edge)?;
        // Before the retirement: a pending list drops archived pairs.
        self.settle_linked_suspects(&edge)?;
        self.retire_superseded(&edge)?;
        self.propagate_from_edge(&edge)?;
        Ok(edge)
    }

    /// A live link says how two notes relate, so a pending suspect between
    /// them is settled by it: `replaces` / `conflicts-with` confirm
    /// it, any other verb dismisses it. A sleep's state-of note raises
    /// suspects against the notes it summarises the moment it is written;
    /// its `builds-on` links clear them.
    fn settle_linked_suspects(&self, edge: &Edge) -> Result<()> {
        if matches!(
            edge.status,
            Some(EdgeStatus::Resolved | EdgeStatus::Dismissed)
        ) {
            return Ok(());
        }
        let cfg = self.store.config();
        let verb = edge.edge_type.as_str();
        let status = if verb == cfg.supersession_verb() || verb == cfg.contradiction_verb() {
            SuspectStatus::Confirmed
        } else {
            SuspectStatus::Dismissed
        };
        let pair = |a: &str, b: &str| {
            (a == edge.from_id && b == edge.to_id) || (a == edge.to_id && b == edge.from_id)
        };
        let mut settled = false;
        for s in self.store.suspects_pending()? {
            if pair(&s.a.id, &s.b.id) {
                self.store.set_suspect_status(&s.id, status)?;
                settled = true;
            }
        }
        if settled {
            self.notify(ChangeEvent::SuspectsChanged);
        }
        Ok(())
    }

    /// Live edges per node id (resolved/dismissed edges don't count) — the
    /// connectedness `canon_order: connected` weighs.
    pub(super) fn live_degrees(&self) -> std::collections::HashMap<String, usize> {
        let mut out = std::collections::HashMap::new();
        for e in self.store.all_edges().unwrap_or_default() {
            if matches!(e.status, Some(EdgeStatus::Resolved | EdgeStatus::Dismissed)) {
                continue;
            }
            *out.entry(e.from_id).or_insert(0) += 1;
            *out.entry(e.to_id).or_insert(0) += 1;
        }
        out
    }

    /// A judged contradiction or supersession reaches whatever depended on
    /// its target: the edge's source now disagrees with, or replaces,
    /// the target, so every note that `inherits` from the target (builds on
    /// it, needs it, holds because of it) is queued against the source. A
    /// tombstone's `replaces` doesn't propagate — burying knowledge is the
    /// user's gesture, not a new claim to hold dependents against.
    fn propagate_from_edge(&self, edge: &Edge) -> Result<()> {
        let cfg = self.store.config();
        let verb = edge.edge_type.as_str();
        if (verb != cfg.supersession_verb() && verb != cfg.contradiction_verb())
            || matches!(
                edge.status,
                Some(EdgeStatus::Resolved | EdgeStatus::Dismissed)
            )
        {
            return Ok(());
        }
        let Some(source) = self.store.get_node(&edge.from_id)? else {
            return Ok(());
        };
        if is_tombstone(&cfg, &source) {
            return Ok(());
        }
        let score = edge.confidence.unwrap_or(1.0);
        if self.propagate_inherited(&source, &edge.to_id, score)? > 0 {
            self.notify(ChangeEvent::SuspectsChanged);
        }
        Ok(())
    }

    /// Queue `(newer, dependent)` for every live note that `inherits` from
    /// `upstream` (an incoming edge whose verb carries the role) — the
    /// transitive contradiction a pairwise judge cannot see, because the two
    /// notes name different subjects and only the edge says one holds what
    /// the other holds. The hint label is `inherited`; its score is the
    /// strength of the upstream contradiction. Returns how many were queued.
    pub(super) fn propagate_inherited(
        &self,
        newer: &Node,
        upstream: &str,
        score: f64,
    ) -> Result<usize> {
        let cfg = self.store.config();
        let newer_vec = self.store.embedding_of(&newer.id)?;
        let mut added = 0;
        for e in self.store.edges_in(upstream)? {
            if e.from_id == newer.id
                || !cfg.verb_inherits(e.edge_type.as_str())
                || matches!(e.status, Some(EdgeStatus::Resolved | EdgeStatus::Dismissed))
            {
                continue;
            }
            let Some(dependent) = self.store.get_node(&e.from_id)? else {
                continue;
            };
            if dependent.valid_until.is_some()
                || is_anchor(&cfg, &dependent)
                || is_tombstone(&cfg, &dependent)
                || self.store.pair_linked(&newer.id, &dependent.id)?
                || self.store.suspect_between(&newer.id, &dependent.id)?
            {
                continue;
            }
            let similarity = match (&newer_vec, self.store.embedding_of(&dependent.id)?) {
                (Some(a), Some(b)) => cosine(a, &b),
                _ => 0.0,
            };
            let (a, b) = if newer.created_at >= dependent.created_at {
                (&newer.id, &dependent.id)
            } else {
                (&dependent.id, &newer.id)
            };
            self.store
                .add_suspect(a, b, similarity, Some(("inherited", score, None)))?;
            added += 1;
        }
        Ok(added)
    }

    /// Supersession is retirement: wherever a live `replaces` edge lands — a
    /// suspect verdict, an assistant `link`, a retype in the pane — the node
    /// it replaces leaves the canon. It is archived, so retrieval, the brief
    /// and the pane stop offering it, and it survives in the `replaces` chain
    /// (`timeline`) where superseded knowledge belongs.
    ///
    /// Two deliberate asymmetries: pinned nodes are exempt (a pin is the
    /// user's "never fade" — the pane's replaces verdict overrides it
    /// explicitly, see [`Engine::resolve_suspect`]), and withdrawing the edge
    /// never un-archives. `valid_until` is also set by decay and by the user,
    /// so restoring it here would resurrect nodes nothing asked to see again.
    pub(super) fn retire_superseded(&self, edge: &Edge) -> Result<()> {
        if edge.edge_type.as_str() != self.store.config().supersession_verb()
            || matches!(
                edge.status,
                Some(EdgeStatus::Resolved | EdgeStatus::Dismissed)
            )
        {
            return Ok(());
        }
        self.archive_superseded(&edge.to_id)?;
        Ok(())
    }

    /// Archive one superseded node; `false` when there was nothing to do
    /// (already archived, pinned, or gone).
    fn archive_superseded(&self, id: &str) -> Result<bool> {
        let Some(node) = self.store.get_node(id)? else {
            return Ok(false);
        };
        if node.valid_until.is_some() || node.trust_override.is_some() {
            return Ok(false);
        }
        self.update_node(
            id,
            NodePatch {
                valid_until: Some(crate::store::now()),
                ..NodePatch::default()
            },
        )?;
        Ok(true)
    }

    /// Heal graphs written before supersession retired its target: archive
    /// every still-current node sitting under a live `replaces` edge. Runs
    /// with the session-boundary validation and is idempotent — after the
    /// first pass it finds nothing.
    pub(super) fn retire_superseded_sweep(&self) -> Result<usize> {
        let verb = self.store.config().supersession_verb().to_string();
        let mut retired = 0;
        for edge in self.store.all_edges()? {
            if edge.edge_type.as_str() != verb
                || matches!(
                    edge.status,
                    Some(EdgeStatus::Resolved | EdgeStatus::Dismissed)
                )
            {
                continue;
            }
            if self.archive_superseded(&edge.to_id)? {
                retired += 1;
            }
        }
        Ok(retired)
    }

    /// Keep endpoint demotions in lockstep with the edge's conflict state:
    /// a live `conflicts-with` is the evidence event that starts decay on the
    /// older claim — stable knowledge loses trust to evidence, never to time
    /// — and evidence that is withdrawn (edge resolved, dismissed, retyped,
    /// deleted) must take its demotion with it, or an innocent node keeps
    /// decaying after the contradiction is gone. (Pinned nodes are skipped
    /// inside demote.)
    pub(super) fn reconcile_conflict_demotion(&self, edge: &Edge) -> Result<()> {
        let live = edge.edge_type.as_str() == self.store.config().contradiction_verb()
            && !matches!(
                edge.status,
                Some(EdgeStatus::Resolved | EdgeStatus::Dismissed)
            );
        if live {
            if let (Some(a), Some(b)) = (
                self.store.get_node(&edge.from_id)?,
                self.store.get_node(&edge.to_id)?,
            ) {
                let older = if a.created_at <= b.created_at { a } else { b };
                self.demote_node(&older, crate::store::now())?;
            }
        } else {
            for id in [&edge.from_id, &edge.to_id] {
                self.undemote_if_unconflicted(id)?;
            }
        }
        Ok(())
    }

    /// Stamp contradicting evidence on a node, with the journal row and SSE
    /// update a trust change deserves. No-op when already demoted or pinned.
    fn demote_node(&self, before: &Node, ts: i64) -> Result<()> {
        if self.store.demote(&before.id, ts)?
            && let Some(node) = self.store.get_node(&before.id)?
        {
            self.audit_node("demoted", Some(before), Some(&node))?;
            self.notify(ChangeEvent::NodeUpdated(node));
        }
        Ok(())
    }

    /// Clear a node's demotion once no live `conflicts-with` edge touches it.
    fn undemote_if_unconflicted(&self, id: &str) -> Result<()> {
        let Some(before) = self.store.get_node(id)? else {
            return Ok(());
        };
        if before.demoted_at.is_none() || self.store.has_active_conflict(id)? {
            return Ok(());
        }
        let node = self.store.clear_demotion(id)?;
        self.audit_node("undemoted", Some(&before), Some(&node))?;
        self.notify(ChangeEvent::NodeUpdated(node));
        Ok(())
    }

    pub fn update_edge(&self, id: &str, p: EdgePatch) -> Result<Edge> {
        if let Some(t) = &p.edge_type {
            self.check_edge_type(t)?;
        }
        let before = self.store.get_edge(id)?;
        let edge = self.store.update_edge(id, p)?;
        self.audit_edge("updated", before.as_ref(), Some(&edge))?;
        self.notify(ChangeEvent::EdgeUpdated(edge.clone()));
        // Retyping to conflicts-with is evidence arriving; resolving,
        // dismissing, or retyping away is evidence withdrawn.
        self.reconcile_conflict_demotion(&edge)?;
        // Retyping INTO the supersession verb retires the target too.
        self.retire_superseded(&edge)?;
        Ok(edge)
    }

    /// Remove one edge. Unlike node deletion this is open to Claude too —
    /// repairing a mislink must not require the pane.
    pub fn delete_edge(&self, id: &str) -> Result<bool> {
        let before = self.store.get_edge(id)?;
        let removed = self.store.delete_edge(id)?;
        if removed {
            self.audit_edge("deleted", before.as_ref(), None)?;
            self.notify(ChangeEvent::EdgeDeleted(id.to_string()));
            if let Some(b) = &before
                && b.edge_type.as_str() == self.store.config().contradiction_verb()
            {
                for endpoint in [&b.from_id, &b.to_id] {
                    self.undemote_if_unconflicted(endpoint)?;
                }
            }
        }
        Ok(removed)
    }
}
