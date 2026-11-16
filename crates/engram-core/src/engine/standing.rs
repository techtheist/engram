//! A hit's standing: conflicts, supersession and endorsement around it.

use super::*;

impl Engine {
    /// A node's grounded-answer verdict — the same `standing` a
    /// search hit carries, for surfaces that show one node (the pane's
    /// inspector). `None` when the node does not exist.
    pub fn node_standing(&self, id: &str) -> Result<Option<Standing>> {
        let Some(node) = self.store.get_node(id)? else {
            return Ok(None);
        };
        let tombstone = is_tombstone(&self.store.config(), &node);
        self.standing_of(id, tombstone, node.stale).map(Some)
    }

    /// How far a hit can be relied on, worst first: tombstone, superseded, open
    /// conflict, stale, then the trust ladder. Reads full edge lists, not the
    /// capped neighbour sample.
    pub(super) fn standing_of(&self, id: &str, tombstone: bool, stale: bool) -> Result<Standing> {
        let standing = |verdict: &str, reason: String, other: Option<String>| Standing {
            verdict: verdict.to_string(),
            reason,
            other,
        };
        if tombstone {
            return Ok(standing(
                "tombstone",
                "a record that this knowledge was deliberately removed — don't act on it or re-add it"
                    .into(),
                None,
            ));
        }
        let cfg = self.store.config();
        let live =
            |e: &Edge| !matches!(e.status, Some(EdgeStatus::Resolved | EdgeStatus::Dismissed));
        let edges_in = self.store.edges_in(id)?;
        for e in edges_in.iter().filter(|e| live(e)) {
            if e.edge_type.as_str() == cfg.supersession_verb()
                && let Some(newer) = self.store.get_node(&e.from_id)?
                && newer.valid_until.is_none()
            {
                return Ok(standing(
                    "superseded",
                    format!("replaced by \"{}\" — read that instead", newer.title),
                    Some(newer.id),
                ));
            }
        }
        let edges_out = self.store.edges_out(id)?;
        for e in edges_in.iter().chain(&edges_out).filter(|e| live(e)) {
            if e.edge_type.as_str() != cfg.contradiction_verb() {
                continue;
            }
            let other_id = if e.from_id == id {
                &e.to_id
            } else {
                &e.from_id
            };
            if let Some(other) = self.store.get_node(other_id)?
                && other.valid_until.is_none()
            {
                return Ok(standing(
                    "contested",
                    format!(
                        "in an open judged conflict with \"{}\" — tell the user before relying on either",
                        other.title
                    ),
                    Some(other.id),
                ));
            }
        }
        if stale {
            return Ok(standing(
                "stale",
                "its trust fell below the stale line — verify against the code or the user first"
                    .into(),
                None,
            ));
        }
        let Some(node) = self.store.get_node(id)? else {
            return Ok(standing(
                "unverified",
                "not found in the store".into(),
                None,
            ));
        };
        Ok(if node.trust_override.is_some() {
            standing("canon", "pinned by the user".into(), None)
        } else if node.approved_at.is_some() {
            standing(
                "canon",
                if node.source == Source::User {
                    "written by the user".into()
                } else {
                    "approved by the user".into()
                },
                None,
            )
        } else if node.confirmed_at.is_some() {
            standing(
                "confirmed",
                "confirmed still true since it was written".into(),
                None,
            )
        } else {
            standing(
                "unverified",
                "written by an assistant and never confirmed — a lead, not a fact".into(),
                None,
            )
        })
    }

    /// Among delivered twins (cosine ≥ `band`), the endorsed one goes first:
    /// pinned, then trust, then the most recent endorsement. Each twin group is
    /// re-sorted within the slots it holds, so nothing else moves.
    pub(super) fn order_twins(&self, hits: &mut [SearchHit], band: f64) -> Result<()> {
        let n = hits.len();
        if n < 2 {
            return Ok(());
        }
        let mut vecs = Vec::with_capacity(n);
        let mut nodes = Vec::with_capacity(n);
        for h in hits.iter() {
            vecs.push(self.store.embedding_of(&h.id)?);
            nodes.push(self.store.get_node(&h.id)?);
        }
        let rung = |i: usize| nodes[i].as_ref().map_or(Rung::None, Rung::of);
        // Union-find over the twin relation, so a chain of twins is one group.
        let mut group: Vec<usize> = (0..n).collect();
        fn root(g: &mut [usize], mut i: usize) -> usize {
            while g[i] != i {
                g[i] = g[g[i]];
                i = g[i];
            }
            i
        }
        for i in 0..n {
            for j in (i + 1)..n {
                if let (Some(a), Some(b)) = (&vecs[i], &vecs[j])
                    && cosine(a, b) >= band
                {
                    let (ri, rj) = (root(&mut group, i), root(&mut group, j));
                    group[rj] = ri;
                }
            }
        }
        let mut members: std::collections::HashMap<usize, Vec<usize>> =
            std::collections::HashMap::new();
        for i in 0..n {
            let r = root(&mut group, i);
            members.entry(r).or_default().push(i);
        }
        let original: Vec<SearchHit> = hits.to_vec();
        for slots in members.values().filter(|m| m.len() > 1) {
            let mut ranked = slots.clone();
            // The endorsement clock is read only for twins that share a rung
            // — one journal lookup each, and only in a twin group.
            let mut clock = std::collections::HashMap::new();
            for &i in slots {
                if let Some(node) = &nodes[i] {
                    clock.insert(i, self.endorsement_clock(node, rung(i))?);
                }
            }
            // Stable: equal standing keeps the vote's order.
            ranked.sort_by(|a, b| {
                let pinned = |i: usize| rung(i) == Rung::Pinned;
                pinned(*b)
                    .cmp(&pinned(*a))
                    .then(original[*b].trust.total_cmp(&original[*a].trust))
                    .then_with(|| {
                        if rung(*a) != rung(*b) {
                            return std::cmp::Ordering::Equal;
                        }
                        clock.get(b).cmp(&clock.get(a))
                    })
            });
            for (slot, from) in slots.iter().zip(ranked) {
                hits[*slot] = original[from].clone();
            }
        }
        Ok(())
    }

    /// When `node` last got the endorsement its rung names, as (stamp, journal
    /// seq). Pins read the newest `pinned` row; approvals and confirmations read
    /// the node stamp plus the matching row's seq.
    fn endorsement_clock(&self, node: &Node, rung: Rung) -> Result<(i64, i64)> {
        let (stamp, action) = match rung {
            Rung::None => return Ok((0, 0)),
            Rung::Pinned => (None, "pinned"),
            Rung::Approved => (node.approved_at, "approved"),
            Rung::Confirmed => (node.confirmed_at, "updated"),
        };
        let newest = self
            .store
            .audit_rows(&AuditQuery {
                entity_id: Some(node.id.clone()),
                action: Some(action.to_string()),
                ..Default::default()
            })?
            .pop();
        let seq = newest.as_ref().map_or(0, |r| r.seq);
        let stamp = stamp.or(newest.map(|r| r.ts)).unwrap_or(0);
        Ok((stamp, seq))
    }
}
