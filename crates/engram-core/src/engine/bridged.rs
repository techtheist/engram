//! Bridge notes: found bridges for display and bridged contradictions for the queue.

use super::*;

impl Engine {
    /// Queue judged bridged pairs and add their views to a write's verdict:
    /// the pair may not involve the written note at all (a new bridge ties
    /// two older facts), and the writer is the one who should judge it.
    pub(super) fn queue_judged_bridges(
        &self,
        judged: Vec<(BridgeCandidate, f64)>,
        views: &mut Vec<SuspectView>,
    ) -> Result<()> {
        let ids = self.queue_bridges(judged)?;
        if ids.is_empty() {
            return Ok(());
        }
        self.notify(ChangeEvent::SuspectsChanged);
        for s in self.store.suspects_pending()? {
            if ids.contains(&s.id) && !views.iter().any(|v| v.id == s.id) {
                views.push(s);
            }
        }
        Ok(())
    }

    /// Bridged pairs to judge for `node`, closest first, capped.
    /// Forward: `node` is a bridge — pair the facts about the subjects it
    /// names. Backward: `node` is a fact — older bridges that name its
    /// subject pair it with the facts about their other subjects. Store
    /// reads only (a title scan over the live notes); judged off the lock by
    /// [`judge_bridges`]. Empty without the logic layer or with the path off.
    pub(super) fn bridge_candidates(&self, node: &Node) -> Result<Vec<BridgeCandidate>> {
        let cfg = self.store.config();
        if cfg.policy.conflict_bridge_gate.is_none()
            || self.nli.is_none()
            || is_anchor(&cfg, node)
            || is_tombstone(&cfg, node)
            || node.valid_until.is_some()
        {
            return Ok(Vec::new());
        }
        let notes = self.store.scannable_nodes()?;
        // Two facts are a pair only when the bridge reaches them through
        // different words (it ties two subjects, not one) and their own
        // titles don't share a subject.
        let differ = |x: &Node, y: &Node| shared_subject(&x.title, &y.title).is_none();
        let mut raw: Vec<(&Node, &Node, &Node)> = Vec::new();
        if crate::bridges::is_bridge(&node.title) {
            let facts = bridge_facts(&notes, node);
            for (i, (x, ax)) in facts.iter().enumerate() {
                for (y, ay) in &facts[i + 1..] {
                    if ax.is_disjoint(ay) && differ(x, y) {
                        raw.push((node, x, y));
                    }
                }
            }
        } else {
            for b in notes
                .iter()
                .filter(|b| b.id != node.id && crate::bridges::is_bridge(&b.title))
            {
                let ax = crate::bridges::anchors(&b.title, &node.title);
                if ax.is_empty() {
                    continue;
                }
                for (y, ay) in bridge_facts(&notes, b) {
                    if y.id != node.id && ax.is_disjoint(&ay) && differ(node, y) {
                        raw.push((b, node, y));
                    }
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (bridge, x, y) in raw {
            let key = if x.id < y.id {
                (x.id.clone(), y.id.clone())
            } else {
                (y.id.clone(), x.id.clone())
            };
            if !seen.insert(key)
                || crate::bridges::restates(&bridge.title, &x.title, &y.title)
                || self.pair_blocked(x, y)?
            {
                continue;
            }
            let similarity = match (
                self.store.embedding_of(&x.id)?,
                self.store.embedding_of(&y.id)?,
            ) {
                (Some(a), Some(b)) => cosine(&a, &b),
                _ => 0.0,
            };
            out.push(BridgeCandidate {
                bridge: bridge.clone(),
                x: x.clone(),
                y: y.clone(),
                similarity,
            });
        }
        out.sort_by(|a, b| b.similarity.total_cmp(&a.similarity));
        out.truncate(crate::policy::BRIDGE_CHECK_CAP);
        Ok(out)
    }

    /// Found bridges for one note, closest first, at most `limit`:
    /// notes a bridge note ties to this one while no edge joins them. For a
    /// bridge, the notes it names; for a fact, the facts about the other
    /// subjects of every bridge that names its own. Title reading only — no
    /// judge, nothing written. Empty for an archived, anchor or tombstone
    /// note.
    pub fn found_bridges(&self, id: &str, limit: usize) -> Result<Vec<FoundBridge>> {
        Ok(self
            .found_bridges_many(&[id.to_string()], limit)?
            .into_iter()
            .next()
            .unwrap_or_default())
    }

    /// [`Engine::found_bridges`] for several notes over one read of the
    /// live notes, in the order asked.
    pub fn found_bridges_many(
        &self,
        ids: &[String],
        limit: usize,
    ) -> Result<Vec<Vec<FoundBridge>>> {
        if limit == 0 || ids.is_empty() {
            return Ok(vec![Vec::new(); ids.len()]);
        }
        let notes = self.store.scannable_nodes()?;
        let bridges: Vec<&Node> = notes
            .iter()
            .filter(|n| crate::bridges::is_bridge(&n.title))
            .collect();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(node) = notes.iter().find(|n| &n.id == id) else {
                out.push(Vec::new()); // archived, anchor, tombstone, or gone
                continue;
            };
            out.push(self.found_bridges_in(&notes, &bridges, node, limit)?);
        }
        Ok(out)
    }

    fn found_bridges_in(
        &self,
        notes: &[Node],
        bridges: &[&Node],
        node: &Node,
        limit: usize,
    ) -> Result<Vec<FoundBridge>> {
        let endpoint = |n: &Node| SuspectEndpoint {
            id: n.id.clone(),
            node_type: n.node_type.clone(),
            title: n.title.clone(),
        };
        let mut raw: Vec<(&Node, Option<&Node>)> = Vec::new();
        if crate::bridges::is_bridge(&node.title) {
            raw.extend(
                pair_bridge_facts(notes, node)
                    .into_iter()
                    .map(|(f, _)| (f, None)),
            );
        } else {
            for b in bridges.iter().filter(|b| b.id != node.id) {
                let ax = crate::bridges::anchors(&b.title, &node.title);
                if ax.is_empty() {
                    continue;
                }
                for (y, ay) in pair_bridge_facts(notes, b) {
                    if ax.is_disjoint(&ay) && shared_subject(&node.title, &y.title).is_none() {
                        raw.push((y, Some(*b)));
                    }
                }
            }
        }
        let mine = self.store.embedding_of(&node.id)?;
        let mut seen = std::collections::HashSet::new();
        let mut found: Vec<(f64, FoundBridge)> = Vec::new();
        for (other, via) in raw {
            if other.id == node.id
                || !seen.insert(other.id.clone())
                || self.store.pair_linked(&node.id, &other.id)?
            {
                continue;
            }
            let similarity = match (&mine, self.store.embedding_of(&other.id)?) {
                (Some(a), Some(b)) => cosine(a, &b),
                _ => 0.0,
            };
            found.push((
                similarity,
                FoundBridge {
                    note: endpoint(other),
                    via: via.map(endpoint),
                },
            ));
        }
        found.sort_by(|a, b| b.0.total_cmp(&a.0));
        Ok(found.into_iter().take(limit).map(|(_, f)| f).collect())
    }

    /// Queue judged bridged pairs, each re-checked under the lock (either
    /// side or the bridge may have changed while the judge ran). Returns the
    /// new suspects' ids.
    fn queue_bridges(&self, judged: Vec<(BridgeCandidate, f64)>) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        for (c, score) in judged {
            let (Some(x), Some(y), Some(bridge)) = (
                self.store.get_node(&c.x.id)?,
                self.store.get_node(&c.y.id)?,
                self.store.get_node(&c.bridge.id)?,
            ) else {
                continue;
            };
            if x.valid_until.is_some()
                || bridge.valid_until.is_some()
                || self.pair_blocked(&x, &y)?
            {
                continue;
            }
            let (newer, older) = if x.created_at >= y.created_at {
                (&x, &y)
            } else {
                (&y, &x)
            };
            let s = self.store.add_suspect(
                &newer.id,
                &older.id,
                c.similarity,
                Some(("bridged", score, None)),
            )?;
            self.store.set_suspect_via(&s.id, &bridge.id)?;
            ids.push(s.id);
        }
        Ok(ids)
    }

    /// The conflict sweep's bridge pass (see [`Engine::audit_conflicts`]).
    /// Pairs judged without queueing are remembered under the judge and the
    /// gate, as the similarity pass remembers its own.
    pub(super) fn bridge_sweep(&self, sweep: &mut AuditSweep, budget: usize) -> Result<()> {
        let cfg = self.store.config();
        let Some(gate) = cfg.policy.conflict_bridge_gate else {
            return Ok(());
        };
        let nli = self.require_nli()?;
        let mut memory = SweepMemory::load(
            self.store.as_ref(),
            "bridged",
            &format!("{}|{gate}", nli.model_id()),
        )?;
        let bridges: Vec<Node> = self
            .store
            .scannable_nodes()?
            .into_iter()
            .filter(|n| crate::bridges::is_bridge(&n.title))
            .collect();
        for bridge in bridges {
            let mut candidates = self.bridge_candidates(&bridge)?;
            candidates.retain(|c| {
                if memory.contains(bridge_memo(c)) {
                    sweep.already_judged += 1;
                    false
                } else {
                    true
                }
            });
            if candidates.is_empty() {
                continue;
            }
            let room = budget.saturating_sub(sweep.examined);
            if candidates.len() > room {
                candidates.truncate(room);
                sweep.truncated = true;
            }
            if candidates.is_empty() {
                break;
            }
            sweep.examined += candidates.len();
            let memos: Vec<u64> = candidates.iter().map(bridge_memo).collect();
            let judged = judge_bridges(Some(nli), Some(gate), candidates);
            let raised: std::collections::HashSet<u64> =
                judged.iter().map(|(c, _)| bridge_memo(c)).collect();
            for m in memos.into_iter().filter(|m| !raised.contains(m)) {
                memory.insert(m);
            }
            let ids = self.queue_bridges(judged)?;
            sweep.queued += ids.len();
            sweep.bridged += ids.len();
            if sweep.truncated {
                break;
            }
        }
        memory.save(self.store.as_ref())?;
        if sweep.bridged > 0 {
            self.notify(ChangeEvent::SuspectsChanged);
        }
        Ok(())
    }
}

/// One bridged pair: `bridge` ties the subject of `x` to the
/// subject of `y`; the judge reads "bridge. y ⟹ x". `similarity` is the
/// pair's own cosine, stored on the suspect like any other.
pub(super) struct BridgeCandidate {
    pub(super) bridge: Node,
    pub(super) x: Node,
    pub(super) y: Node,
    pub(super) similarity: f64,
}

/// The words through which a bridge reaches a fact (see
/// [`crate::bridges::anchors`]); a fact reached one chain step away is
/// anchored by the hop bridge it came through.
pub(super) type Anchor = std::collections::HashSet<String>;

/// The live plain facts a bridge names, each with its anchor, plus — one
/// chain step — the facts named by any bridge it names. Bridges themselves
/// are never facts.
fn bridge_facts<'a>(notes: &'a [Node], bridge: &Node) -> Vec<(&'a Node, Anchor)> {
    let named = |b: &Node| -> Vec<(&'a Node, Anchor)> {
        notes
            .iter()
            .filter(|n| n.id != b.id)
            .filter_map(|n| {
                let a = crate::bridges::anchors(&b.title, &n.title);
                (!a.is_empty()).then_some((n, a))
            })
            .collect()
    };
    let mut out: Vec<(&Node, Anchor)> = Vec::new();
    for (n, anchor) in named(bridge) {
        if crate::bridges::is_bridge(&n.title) {
            for (f, _) in named(n) {
                if f.id != bridge.id && !out.iter().any(|(o, _)| o.id == f.id) {
                    out.push((f, [format!("hop:{}", n.id)].into()));
                }
            }
        } else if !out.iter().any(|(o, _)| o.id == n.id) {
            out.push((n, anchor));
        }
    }
    out.retain(|(n, _)| !crate::bridges::is_bridge(&n.title));
    out
}

/// [`bridge_facts`] for display (found bridges): only when the bridge names
/// exactly two subjects. Facts whose anchors overlap are one subject; a
/// title that reaches three or more is prose that says "same" or "shared"
/// in passing, not a tie between two things — the judge filters those on
/// the write path, but nothing judges what is only shown.
///
/// And the two must sit in the tie's frame ([`crate::bridges::frame`]): one
/// subject before the cue, the other after the word that closes it — so
/// "one shared component" or "scores the same" pair nothing. A subject
/// reached through a chain hop stands on whichever side is left.
fn pair_bridge_facts<'a>(notes: &'a [Node], bridge: &Node) -> Vec<(&'a Node, Anchor)> {
    let Some(sides) = crate::bridges::frame(&bridge.title) else {
        return Vec::new();
    };
    let facts = bridge_facts(notes, bridge);
    let mut groups: Vec<Anchor> = Vec::new();
    for (_, a) in &facts {
        let (joined, mut rest): (Vec<Anchor>, Vec<Anchor>) =
            groups.into_iter().partition(|g| !g.is_disjoint(a));
        let mut merged = a.clone();
        for g in joined {
            merged.extend(g);
        }
        rest.push(merged);
        groups = rest;
    }
    if groups.len() != 2 {
        return Vec::new();
    }
    // Which stretch of the frame a subject stands in: None for a chain hop
    // (it stands wherever the other does not), Err when its words span
    // stretches or none — not a framed subject.
    let side = |g: &Anchor| -> std::result::Result<Option<usize>, ()> {
        let words: Vec<&String> = g.iter().filter(|w| !w.starts_with("hop:")).collect();
        if words.is_empty() {
            return Ok(None);
        }
        sides
            .iter()
            .position(|s| words.iter().all(|w| s.contains(*w)))
            .map(Some)
            .ok_or(())
    };
    let framed = match (side(&groups[0]), side(&groups[1])) {
        (Ok(Some(a)), Ok(Some(b))) => a != b,
        (Ok(None), Ok(Some(_))) | (Ok(Some(_)), Ok(None)) => true,
        _ => false,
    };
    if framed { facts } else { Vec::new() }
}

/// A bridged pair's sweep memory key: both facts' content and the bridge's.
fn bridge_memo(c: &BridgeCandidate) -> u64 {
    pair_memo(
        &c.x,
        &c.y,
        &format!(
            "bridge:{}:{:x}",
            c.bridge.id,
            content_hash(&c.bridge.title, c.bridge.body.as_deref())
        ),
    )
}

/// Judge bridged pairs — no engine, no lock. One direction per pair,
/// "bridge. y ⟹ x"; a pair queues at or above `gate`. Every judge gets the
/// same question.
pub(super) fn judge_bridges(
    nli: Option<&dyn Nli>,
    gate: Option<f64>,
    candidates: Vec<BridgeCandidate>,
) -> Vec<(BridgeCandidate, f64)> {
    let (Some(nli), Some(gate)) = (nli, gate) else {
        return Vec::new();
    };
    if candidates.is_empty() {
        return Vec::new();
    }
    let pairs: Vec<(String, String)> = candidates
        .iter()
        .map(|c| {
            (
                format!("{}. {}", c.bridge.title.trim(), c.y.title.trim()),
                c.x.title.trim().to_string(),
            )
        })
        .collect();
    let Ok(judged) = nli.judge(&pairs) else {
        return Vec::new();
    };
    candidates
        .into_iter()
        .zip(judged)
        .filter_map(|(c, j)| {
            let score = f64::from(j.contradiction);
            (score >= gate).then_some((c, score))
        })
        .collect()
}
