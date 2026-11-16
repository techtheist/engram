//! Merging restated notes into one survivor.

use super::*;

impl Engine {
    /// Merge duplicates into one survivor: tags and code_refs union, live edges
    /// rehome, and each victim is superseded by a `replaces` edge. Nothing is
    /// judged wrong and nothing is deleted.
    ///
    /// Dead and internal edges stay on their victim; an edge the survivor already
    /// has is skipped; incoming supersessions never move. Rehomed edges keep their
    /// id, and a rehomed contradiction re-runs demotion. Pinned victims are refused
    /// for the assistant and archived for a user merge. Not atomic: each step
    /// journals, so a failure leaves a smaller valid merge.
    pub fn merge_nodes(
        &self,
        survivor_id: &str,
        victims: &[String],
        title: Option<String>,
        body: Option<String>,
        source: Source,
    ) -> Result<MergeOutcome> {
        // One operation in the journal: undo reverts all of it.
        let _op = self.op_scope();
        let survivor = self
            .store
            .get_node(survivor_id)?
            .ok_or_else(|| crate::Error::NotFound(survivor_id.to_string()))?;
        if survivor.valid_until.is_some() {
            return Err(crate::Error::Parse {
                kind: "merge",
                value: format!(
                    "survivor {survivor_id} is archived — merge into the live generation"
                ),
            });
        }
        let mut victim_nodes: Vec<Node> = Vec::new();
        for id in victims {
            if id == survivor_id || victim_nodes.iter().any(|v| &v.id == id) {
                continue;
            }
            let node = self
                .store
                .get_node(id)?
                .ok_or_else(|| crate::Error::NotFound(id.clone()))?;
            if source == Source::Agent && node.trust_override.is_some() {
                return Err(crate::Error::Pinned(format!(
                    "\"{}\" ({}) is user-pinned; merging would archive it — \
                     tell the user and let them merge this pair in the pane",
                    node.title, node.id
                )));
            }
            victim_nodes.push(node);
        }
        if victim_nodes.is_empty() {
            return Err(crate::Error::Parse {
                kind: "merge",
                value: "no victims to merge (the survivor itself doesn't count)".into(),
            });
        }
        let victim_ids: Vec<String> = victim_nodes.iter().map(|v| v.id.clone()).collect();

        // The union the survivor will carry.
        let mut tags = survivor.tags.clone();
        let mut code_refs = survivor.code_refs.clone();
        for v in &victim_nodes {
            for t in &v.tags {
                if !tags.contains(t) {
                    tags.push(t.clone());
                }
            }
            for r in &v.code_refs {
                if !code_refs.contains(r) {
                    code_refs.push(r.clone());
                }
            }
        }

        // The survivor's live (verb, far-endpoint) pairs, per direction —
        // grown as edges rehome so two victims can't both move the same link.
        let live = |e: &Edge| e.valid_until.is_none();
        let mut out_keys: std::collections::HashSet<(String, String)> = self
            .store
            .edges_out(survivor_id)?
            .iter()
            .filter(|e| live(e))
            .map(|e| (e.edge_type.as_str().to_string(), e.to_id.clone()))
            .collect();
        let mut in_keys: std::collections::HashSet<(String, String)> = self
            .store
            .edges_in(survivor_id)?
            .iter()
            .filter(|e| live(e))
            .map(|e| (e.edge_type.as_str().to_string(), e.from_id.clone()))
            .collect();

        let supersession = self.store.config().supersession_verb().to_string();
        let mut merged = Vec::new();
        for victim in &victim_nodes {
            let mut rehomed_edges = 0usize;
            let mut skipped_edges = 0usize;
            for edge in self.store.edges_out(&victim.id)? {
                if !live(&edge) {
                    continue;
                }
                let key = (edge.edge_type.as_str().to_string(), edge.to_id.clone());
                if edge.to_id == survivor_id
                    || victim_ids.contains(&edge.to_id)
                    || out_keys.contains(&key)
                {
                    skipped_edges += 1;
                    continue;
                }
                let before = edge.clone();
                let mut moved = edge;
                moved.from_id = survivor_id.to_string();
                self.store.upsert_edge(&moved)?;
                self.audit_edge("updated", Some(&before), Some(&moved))?;
                self.notify(ChangeEvent::EdgeUpdated(moved.clone()));
                self.reconcile_conflict_demotion(&moved)?;
                self.retire_superseded(&moved)?;
                out_keys.insert(key);
                rehomed_edges += 1;
            }
            for edge in self.store.edges_in(&victim.id)? {
                if !live(&edge) {
                    continue;
                }
                let key = (edge.edge_type.as_str().to_string(), edge.from_id.clone());
                if edge.edge_type.as_str() == supersession
                    || edge.from_id == survivor_id
                    || victim_ids.contains(&edge.from_id)
                    || in_keys.contains(&key)
                {
                    skipped_edges += 1;
                    continue;
                }
                let before = edge.clone();
                let mut moved = edge;
                moved.to_id = survivor_id.to_string();
                self.store.upsert_edge(&moved)?;
                self.audit_edge("updated", Some(&before), Some(&moved))?;
                self.notify(ChangeEvent::EdgeUpdated(moved.clone()));
                self.reconcile_conflict_demotion(&moved)?;
                in_keys.insert(key);
                rehomed_edges += 1;
            }
            // The story edge — skipped when a previous merge already wrote it.
            let replaces_key = (supersession.clone(), victim.id.clone());
            if !out_keys.contains(&replaces_key) {
                self.add_edge(NewEdge {
                    edge_type: self.store.config().supersession_edge(),
                    from_id: survivor_id.to_string(),
                    to_id: victim.id.clone(),
                    source,
                    note: Some("merged".into()),
                    confidence: None,
                    strength: None,
                    status: None,
                })?;
                out_keys.insert(replaces_key);
            }
            // add_edge retired the victim unless it is pinned; a USER merge
            // overrides the pin (the assistant case errored above).
            if let Some(current) = self.store.get_node(&victim.id)?
                && current.valid_until.is_none()
            {
                self.update_node(
                    &victim.id,
                    NodePatch {
                        valid_until: Some(crate::store::now()),
                        ..NodePatch::default()
                    },
                )?;
            }
            let after = self.store.get_node(&victim.id)?;
            self.audit_node("merged", Some(victim), after.as_ref())?;
            merged.push(MergedVictim {
                id: victim.id.clone(),
                title: victim.title.clone(),
                rehomed_edges,
                skipped_edges,
            });
        }

        // The survivor's convergence is a deliberate update: it re-embeds,
        // stamps confirmed_at, and earns the same-turn verdict set.
        let checked = self.update_node_checked(
            survivor_id,
            NodePatch {
                title,
                body,
                tags: Some(tags),
                code_refs: Some(code_refs),
                ..NodePatch::default()
            },
        )?;
        let mut warnings = checked.warnings;
        warnings.retain(|w| !victim_ids.contains(&w.id));
        Ok(MergeOutcome {
            survivor: checked.node,
            merged,
            warnings,
            suspects: checked.suspects,
            missing_refs: checked.missing_refs,
            canon: checked.canon,
        })
    }
}
