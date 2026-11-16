//! Born-in provenance: links a curated note to the exchange it came from.

use super::*;

impl Engine {
    /// Park a freshly-created curated note for born-in provenance (decision
    /// 00bgftf9usll): the harvester lags the live transcript, so the note's
    /// birth exchange resolves on a later tick via
    /// [`Engine::resolve_provenance`]. No-op when the history layer is off.
    pub fn park_provenance(&self, note_id: &str, ts: i64) {
        if !self.history_open() {
            return;
        }
        let mut lot = self.provenance_lot.locked();
        // Bounded: a graph nobody harvests must not grow a queue forever.
        if lot.len() >= 256 {
            lot.remove(0);
        }
        lot.push(ParkedProvenance {
            note_id: note_id.to_string(),
            ts,
            parked_at: crate::store::now(),
        });
    }

    /// Re-park recent assistant notes still missing their born-in edge, so a core
    /// restart costs no provenance. Bounded by `window_secs`.
    pub fn repark_recent_provenance(&self, window_secs: i64) -> Result<usize> {
        if !self.history_open() {
            return Ok(0);
        }
        let now = crate::store::now();
        let already: std::collections::HashSet<String> = self
            .provenance_lot
            .locked()
            .iter()
            .map(|p| p.note_id.clone())
            .collect();
        let mut reparked = 0;
        for node in self.store.all_nodes()? {
            if node.source != crate::types::Source::Agent
                || node.valid_until.is_some()
                || now - node.created_at > window_secs
                || already.contains(&node.id)
                || self.born_in_of(&node.id).is_some()
            {
                continue;
            }
            self.park_provenance(&node.id, node.created_at);
            reparked += 1;
        }
        Ok(reparked)
    }

    /// Link parked notes to their birth exchange: the closest preceding assistant
    /// message, preferring sessions alive after the note's moment. An entry leaves
    /// the lot once ingestion passes it or its grace expires. Returns edges written.
    pub fn resolve_provenance(&self) -> Result<usize> {
        const GRACE_SECS: i64 = 900;
        {
            let lot = self.provenance_lot.locked();
            if lot.is_empty() {
                return Ok(0);
            }
        }
        // One history scan amortized over every parked entry.
        let msgs: Vec<(String, i64, bool, Option<String>)> = self
            .with_history(|s| s.all_nodes())
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .filter(|n| n.node_type.as_str() == crate::history::MESSAGE_TYPE)
            .map(|n| {
                let assistant = n
                    .props
                    .as_ref()
                    .and_then(|p| p.get("role"))
                    .and_then(|r| r.as_str())
                    == Some("assistant");
                (n.id, n.created_at, assistant, n.session_id)
            })
            .collect();
        let ingested_up_to = msgs.iter().map(|m| m.1).max().unwrap_or(i64::MIN);
        let now = crate::store::now();
        let mut lot = self.provenance_lot.locked();
        let mut edges = Vec::new();
        lot.retain(|p| {
            if ingested_up_to < p.ts && now - p.parked_at <= GRACE_SECS {
                return true; // the transcript hasn't caught up — wait
            }
            let alive: std::collections::HashSet<&str> = msgs
                .iter()
                .filter(|m| m.1 >= p.ts)
                .filter_map(|m| m.3.as_deref())
                .collect();
            let best = msgs.iter().filter(|m| m.2 && m.1 <= p.ts).max_by_key(|m| {
                let in_alive = m.3.as_deref().is_some_and(|s| alive.contains(s));
                (in_alive, m.1, m.0.clone())
            });
            if let Some(m) = best {
                edges.push((p.note_id.clone(), m.0.clone()));
            }
            false // resolved or expired — either way, done
        });
        drop(lot);
        let n = edges.len();
        let ts = now;
        for (note, msg) in edges {
            // born-in is the one half-resident edge: it lives in the history
            // store but its `from` is a curated node. `add_edge` validates
            // endpoints, so it goes through the verbatim upsert path instead.
            let edge = crate::types::Edge {
                id: crate::id::new_id(),
                edge_type: crate::types::EdgeType::parse(crate::history::VERB_BORN_IN)?,
                from_id: note,
                to_id: msg,
                source: crate::types::Source::Agent,
                created_at: ts,
                confidence: None,
                strength: None,
                note: None,
                valid_from: Some(ts),
                valid_until: None,
                status: None,
            };
            self.with_history(|s| s.upsert_edge(&edge)).transpose()?;
        }
        Ok(n)
    }

    /// Where a curated node was born, when its `born-in` edge exists — the
    /// provenance line on search hits and the pane's history chip.
    pub fn born_in_of(&self, node_id: &str) -> Option<crate::history::BornIn> {
        self.with_history(|s| {
            let edge = s
                .edges_out(node_id)
                .ok()?
                .into_iter()
                .find(|e| e.edge_type.as_str() == crate::history::VERB_BORN_IN)?;
            let msg = s.get_node(&edge.to_id).ok()??;
            Some(crate::history::BornIn {
                session: msg.session_id.clone().unwrap_or_default(),
                turn: msg
                    .props
                    .as_ref()
                    .and_then(|p| p.get("turn"))
                    .and_then(serde_json::Value::as_u64),
                timestamp: msg.created_at,
                message_id: msg.id,
            })
        })
        .flatten()
    }

    /// Curated notes born in one recorded session, in turn order (the reverse of
    /// [`Self::born_in_of`]); `born-in` edges live in the history store.
    pub fn session_notes(&self, session: &str) -> Result<Vec<crate::history::SessionNote>> {
        // (note id, turn, message id, message ts) per born-in edge.
        type Born = Vec<(String, Option<u64>, String, i64)>;
        let born: Born = self
            .with_history(|s| -> Result<Born> {
                let mut out = Vec::new();
                for n in s.nodes_in_session(session)? {
                    if n.node_type.as_str() != crate::history::MESSAGE_TYPE {
                        continue;
                    }
                    let turn = n
                        .props
                        .as_ref()
                        .and_then(|p| p.get("turn"))
                        .and_then(serde_json::Value::as_u64);
                    for e in s.edges_in(&n.id)? {
                        if e.edge_type.as_str() == crate::history::VERB_BORN_IN {
                            out.push((e.from_id.clone(), turn, n.id.clone(), n.created_at));
                        }
                    }
                }
                Ok(out)
            })
            .transpose()?
            .unwrap_or_default();
        let mut notes = Vec::new();
        for (note_id, turn, message_id, timestamp) in born {
            let Some(node) = self.store.get_node(&note_id)? else {
                continue; // the note was hard-deleted; its edge is a ghost
            };
            notes.push(crate::history::SessionNote {
                id: node.id,
                node_type: node.node_type.as_str().to_string(),
                title: node.title,
                turn,
                message_id,
                timestamp,
            });
        }
        notes.sort_by_key(|n| (n.turn.unwrap_or(u64::MAX), n.timestamp));
        Ok(notes)
    }
}
