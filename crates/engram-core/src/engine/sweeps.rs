//! Background sweeps: conflict scans and the Checkup audits.

use super::*;

impl Engine {
    /// Sweep the whole graph for unlinked look-alike pairs (the pane's
    /// "Scan now" and the daemon's periodic pass). Returns how many new
    /// suspects were queued.
    pub fn scan_conflicts(&self) -> Result<usize> {
        let mut added = 0;
        for id in self.scan_ids()? {
            added += self.scan_one(&id)?;
        }
        if added > 0 {
            self.notify(ChangeEvent::SuspectsChanged);
        }
        Ok(added)
    }

    /// The nodes a conflict scan visits, in scan order.
    pub fn scan_ids(&self) -> Result<Vec<String>> {
        Ok(self
            .store
            .scannable_nodes()?
            .into_iter()
            .map(|n| n.id)
            .collect())
    }

    /// Scan one node for suspects; 0 when it is gone, archived or has no
    /// vector (it may have changed since [`Engine::scan_ids`] listed it).
    pub fn scan_one(&self, id: &str) -> Result<usize> {
        let Some(node) = self.store.get_node(id)? else {
            return Ok(0);
        };
        if node.valid_until.is_some() {
            return Ok(0);
        }
        let Some(vec) = self.store.embedding_of(id)? else {
            return Ok(0);
        };
        self.suspects_near(&node, &vec)
    }

    // ---- local cortex, logic layer. All read-only nominations:
    // sweeps queue suspects for judgment, claim checks annotate — no trust
    // field moves here.

    /// Conflict sweep (the Checkup panel's "Find hidden conflicts"): rescan
    /// at the standing similarity threshold, queueing only pairs the NLI
    /// layer marks as contradictions. Above the floor the claim text is
    /// judged; MNLI-class models presuppose co-reference, and below that band
    /// unrelated same-shaped claims read as confident contradictions (the
    /// dogfood finding of 2026-07-13 — 140 junk pairs at a 0.8 gate). Below
    /// it the sweep admits exactly what the write path admits since
    /// 0.9.4: titles that name one subject and share a claim word, read as a
    /// contradiction at `policy.conflict_nli_gate` — so the button reaches
    /// what the background scan reaches, and further down the neighbour list.
    ///
    /// Then the bridge pass: every live bridge note pairs the facts
    /// about the subjects it ties, as a write would — so bridges written
    /// before the path existed, or pairs a write's cap left out, are read
    /// too. It spends what the similarity pass left of the budget.
    pub fn audit_conflicts(&self) -> Result<AuditSweep> {
        let mut sweep = self.audit_sweep(
            "contradiction",
            self.store.config().policy.conflict_suspect_similarity,
            AUDIT_NLI_PAIR_BUDGET,
        )?;
        self.bridge_sweep(&mut sweep, AUDIT_NLI_PAIR_BUDGET)?;
        Ok(sweep)
    }

    /// Duplicate sweep (the Audit panel's "Find duplicates"): mutual
    /// entailment above a 0.80 similarity floor — two nodes stating the same
    /// thing. Queued as suspects; the judge's `replaces` verdict is the merge.
    pub fn audit_duplicates(&self) -> Result<AuditSweep> {
        self.audit_sweep("entailment", 0.80, AUDIT_NLI_PAIR_BUDGET)
    }

    /// Shared sweep: nominate unlinked, unraised look-alike pairs whose NLI
    /// hint matches `target`. NLI pair budget capped — an audit that takes a
    /// minute under the engine lock is worse than one that says "truncated,
    /// run me again". Since 0.9.11 "again" makes progress: each unordered
    /// pair is judged once per run, and a pair judged without queueing is
    /// remembered ([`SweepMemory`]) so the next run spends its budget on
    /// pairs no run has read yet.
    pub(crate) fn audit_sweep(
        &self,
        target: &'static str,
        floor: f64,
        budget: usize,
    ) -> Result<AuditSweep> {
        let cfg = self.store.config();
        let nli = self.require_nli()?;
        // Below the floor only the conflict sweep reads, and only through the
        // title gate — the duplicate sweep's floor is already its band.
        let title_gate = cfg
            .policy
            .conflict_nli_gate
            .filter(|_| target == "contradiction");
        let low = match title_gate {
            Some(_) => crate::policy::CONFLICT_NLI_FLOOR.min(floor),
            None => floor,
        };
        let mut memory = SweepMemory::load(
            self.store.as_ref(),
            target,
            &format!(
                "{}|{:?}|{}",
                nli.model_id(),
                title_gate,
                cfg.policy.nli_sweep_min_confidence
            ),
        )?;
        let mut sweep = AuditSweep::default();
        // The scan meets every close pair from both sides; whatever happened
        // to it the first time (judged, queued, skipped) holds for the second.
        let mut seen = std::collections::HashSet::new();
        // Crowding order: the subjects first, largest first and
        // each from its most central member, then every other note newest
        // first. Conflicts and restatements pile up where notes crowd, so a
        // run the budget cuts has spent itself there. Order only: reading
        // subject-mates beyond the twelve nearest was tried and dropped —
        // on this repo's graph it judged 13 more pairs and raised one more
        // false alarm with Laya, nothing found (2026-11-15 replay).
        let index = self.crowding()?;
        let rank: std::collections::HashMap<String, usize> = index
            .subjects(|_| true)
            .iter()
            .flat_map(|s| s.members.iter())
            .enumerate()
            .map(|(i, m)| (m.clone(), i))
            .collect();
        let mut notes = self.store.scannable_nodes()?;
        // Stable: notes outside every subject keep the store's newest-first.
        notes.sort_by_key(|n| rank.get(&n.id).copied().unwrap_or(usize::MAX));
        'nodes: for node in notes {
            let Some(vec) = self.store.embedding_of(&node.id)? else {
                continue;
            };
            let near = self
                .store
                .search_vec(&vec, 12)?
                .into_iter()
                .map(|(id, d)| (id, 1.0 - d));
            for (id, similarity) in near {
                if id == node.id {
                    continue;
                }
                if similarity < low {
                    break; // distance-ordered
                }
                let key = if node.id < id {
                    (node.id.clone(), id.clone())
                } else {
                    (id.clone(), node.id.clone())
                };
                if !seen.insert(key) {
                    continue;
                }
                let Some(other) = self.store.get_node(&id)? else {
                    continue;
                };
                // Below the floor a pair exists for this sweep only if the
                // title guard admits it; the skip counters describe the close
                // pairs above it.
                let path = if similarity >= floor {
                    None
                } else {
                    match title_pair_admission(&node.title, &other.title) {
                        Some(path) => Some(path),
                        None => continue,
                    }
                };
                let passed_over = |bucket: &mut usize| {
                    if path.is_none() {
                        *bucket += 1;
                    }
                };
                if is_anchor(&cfg, &other)
                    || is_tombstone(&cfg, &other)
                    || other.valid_until.is_some()
                {
                    passed_over(&mut sweep.inactive);
                    continue;
                }
                if self.store.pair_linked(&node.id, &other.id)? {
                    passed_over(&mut sweep.already_linked);
                    continue;
                }
                if self.store.suspect_between(&node.id, &other.id)? {
                    passed_over(&mut sweep.already_raised);
                    continue;
                }
                let memo = pair_memo(&node, &other, path.unwrap_or("claim"));
                if memory.contains(memo) {
                    sweep.already_judged += 1;
                    continue;
                }
                if sweep.examined >= budget {
                    sweep.truncated = true;
                    break 'nodes;
                }
                sweep.examined += 1;
                let hint = match path {
                    None => self.nli_hint(&node, &other),
                    Some(path) => self.nli_title_hint_for(path, &node, &other),
                };
                let Some((label, score, direction)) = hint else {
                    continue; // the judge failed: nothing was read, nothing to remember
                };
                let gate = match path {
                    None => cfg.policy.nli_sweep_min_confidence,
                    Some(_) => title_gate.unwrap_or(f64::INFINITY),
                };
                if label != target || score < gate {
                    memory.insert(memo);
                    continue;
                }
                let (newer, older) = if node.created_at >= other.created_at {
                    (&node.id, &other.id)
                } else {
                    (&other.id, &node.id)
                };
                self.store.add_suspect(
                    newer,
                    older,
                    similarity,
                    Some((label, score, direction)),
                )?;
                sweep.queued += 1;
            }
        }
        memory.save(self.store.as_ref())?;
        if sweep.queued > 0 {
            self.notify(ChangeEvent::SuspectsChanged);
        }
        Ok(sweep)
    }

    /// "Check open problems": does any current node entail an answer to an
    /// open Problem/Intent? Returns nominations — the human (or assistant)
    /// still links `answers` and resolves. Pairs already linked with the
    /// answer-role verb are dropped (nothing to suggest); pairs linked some
    /// OTHER way keep their nomination but rank under a penalty, carrying
    /// the existing verb — "these are connected, but maybe the answer link
    /// is the one that's missing".
    pub fn audit_answered(&self) -> Result<Vec<AnsweredHint>> {
        const NLI_PAIR_BUDGET: usize = 150;
        let nli = self.require_nli()?;
        let cfg = self.store.config();
        let answer_verb = cfg.ontology.verbs.iter().find(|v| v.roles.answer);
        let mut hints = Vec::new();
        let mut examined = 0;
        for problem in self.store.list_open(&[])? {
            let Some(vec) = self.store.embedding_of(&problem.id)? else {
                continue;
            };
            // The problem's incident edges, fetched once for all candidates.
            let mut incident = self.store.edges_out(&problem.id)?;
            incident.extend(self.store.edges_in(&problem.id)?);
            for (id, distance) in self.store.search_vec(&vec, 8)? {
                if id == problem.id || 1.0 - distance < 0.6 {
                    continue;
                }
                let Some(candidate) = self.store.get_node(&id)? else {
                    continue;
                };
                // Answer candidates by role: any non-worklist, non-anchor,
                // non-tombstone type can settle an open item (Resolution/
                // Decision/Insight and the canon types in the shipped set —
                // a removal marker never answers open work).
                let can_answer = cfg
                    .type_def(candidate.node_type.as_str())
                    .is_some_and(|t| !t.roles.worklist && !t.roles.anchor && !t.roles.tombstone);
                if candidate.valid_until.is_some() || !can_answer {
                    continue;
                }
                if examined >= NLI_PAIR_BUDGET {
                    return Ok(hints);
                }
                examined += 1;
                let Ok(j) = nli.judge(&[(claim(&candidate), claim(&problem))]) else {
                    continue;
                };
                let entailment = j[0].entailment;
                if entailment >= 0.6 {
                    // Already linked? With the answer verb: nothing left to
                    // suggest. With another verb: keep the nomination at a
                    // penalty — the connection exists, but the answer link
                    // may be the missing one.
                    let existing: Vec<String> = incident
                        .iter()
                        .filter(|e| e.to_id == candidate.id || e.from_id == candidate.id)
                        .map(|e| e.edge_type.as_str().to_string())
                        .collect();
                    if let Some(av) = answer_verb
                        && existing.contains(&av.name)
                    {
                        continue;
                    }
                    hints.push(AnsweredHint {
                        problem: SuspectEndpoint {
                            id: problem.id.clone(),
                            node_type: problem.node_type.clone(),
                            title: problem.title.clone(),
                        },
                        candidate: SuspectEndpoint {
                            id: candidate.id,
                            node_type: candidate.node_type,
                            title: candidate.title,
                        },
                        entailment: f64::from(entailment),
                        existing_link: existing.into_iter().next(),
                    });
                }
            }
        }
        // Fresh pairs first: an existing (non-answer) link halves the rank.
        let rank =
            |h: &AnsweredHint| h.entailment * if h.existing_link.is_some() { 0.5 } else { 1.0 };
        hints.sort_by(|a, b| rank(b).total_cmp(&rank(a)));
        Ok(hints)
    }

    /// "Triage stale notes": judge each stale node against its nearest live
    /// canon and say what the evidence suggests — `reconfirm` (a current node
    /// still entails it: confirm-still-true restores its trust),
    /// `contradicted` (a current node disputes it — judge as a conflict;
    /// gated on the suspect similarity band because MNLI presupposes
    /// co-reference), or `isolated` (nothing current speaks to it — an
    /// archive candidate). Nominations only; nothing self-applies.
    pub fn audit_stale_triage(&self) -> Result<Vec<StaleTriage>> {
        const NLI_PAIR_BUDGET: usize = 150;
        const TRIAGE_ENTAILMENT: f32 = 0.60;
        let nli = self.require_nli()?;
        let cfg = self.store.config();
        let mut out = Vec::new();
        let mut examined = 0;
        'stale: for node in self.store.recent_nodes(usize::MAX)? {
            if !node.stale || node.valid_until.is_some() || node.trust_override.is_some() {
                continue;
            }
            let endpoint = SuspectEndpoint {
                id: node.id.clone(),
                node_type: node.node_type.clone(),
                title: node.title.clone(),
            };
            let Some(vec) = self.store.embedding_of(&node.id)? else {
                continue;
            };
            let mut spoke = false;
            for (id, distance) in self.store.search_vec(&vec, 6)? {
                let similarity = 1.0 - distance;
                if id == node.id || similarity < 0.6 {
                    continue;
                }
                let Some(other) = self.store.get_node(&id)? else {
                    continue;
                };
                if other.valid_until.is_some() || other.stale {
                    continue;
                }
                if examined >= NLI_PAIR_BUDGET {
                    break 'stale;
                }
                examined += 1;
                let Ok(j) = nli.judge_pair(&claim(&other), &claim(&node)) else {
                    continue;
                };
                let evidence = SuspectEndpoint {
                    id: other.id,
                    node_type: other.node_type,
                    title: other.title,
                };
                if j.contradiction() >= TRIAGE_ENTAILMENT
                    && similarity >= cfg.policy.conflict_suspect_similarity
                {
                    out.push(StaleTriage {
                        node: endpoint.clone(),
                        trust: node.trust,
                        verdict: "contradicted".into(),
                        evidence: Some(evidence),
                        score: f64::from(j.contradiction()),
                    });
                    continue 'stale;
                }
                if j.forward.entailment >= TRIAGE_ENTAILMENT {
                    out.push(StaleTriage {
                        node: endpoint.clone(),
                        trust: node.trust,
                        verdict: "reconfirm".into(),
                        evidence: Some(evidence),
                        score: f64::from(j.forward.entailment),
                    });
                    continue 'stale;
                }
                spoke = true;
            }
            if !spoke {
                out.push(StaleTriage {
                    node: endpoint,
                    trust: node.trust,
                    verdict: "isolated".into(),
                    evidence: None,
                    score: 0.0,
                });
            }
        }
        Ok(out)
    }
}

/// Whether a node's type carries the `tombstone` role under this graph's
/// ontology (a record of deliberately removed knowledge — findable, never
/// canon, never to be re-derived).
/// The endorsement ladder a twin sits on, highest first (twin order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Rung {
    Pinned,
    Approved,
    Confirmed,
    None,
}

impl Rung {
    pub(super) fn of(n: &Node) -> Self {
        if n.trust_override.is_some() {
            Rung::Pinned
        } else if n.approved_at.is_some() {
            Rung::Approved
        } else if n.confirmed_at.is_some() {
            Rung::Confirmed
        } else {
            Rung::None
        }
    }
}

/// NLI pairs one Checkup sweep run may judge. An audit that holds the
/// engine for a minute is worse than one that says "run me again" — and
/// since 0.9.11 running it again continues where the last run stopped.
const AUDIT_NLI_PAIR_BUDGET: usize = 300;

/// Remembered pairs per sweep kind; the oldest fall out past this (a pair
/// read again costs one NLI call, never a wrong answer).
const SWEEP_MEMORY_MAX: usize = 20_000;

/// The pairs a Checkup sweep judged without queueing (0.9.11, Problem
/// 00dxmtfmipga): before it, every run re-read the same first 300 pairs and
/// "run again to continue" never continued. Kept in the store's kv meta per
/// sweep kind, so it travels with `migrate` like the rest of meta. A pair is
/// remembered by [`pair_memo`] — both notes' ids and content plus the read
/// that judged it — so editing either note re-opens it, and the whole
/// memory is dropped when its stamp (judge model, gates) changes: a verdict
/// is only as reusable as the judge and the bar that produced it.
pub(super) struct SweepMemory {
    pub(super) key: String,
    pub(super) stamp: String,
    pub(super) order: Vec<u64>,
    pub(super) set: std::collections::HashSet<u64>,
    pub(super) dirty: bool,
}

impl SweepMemory {
    pub(super) fn load(store: &dyn Store, target: &str, stamp: &str) -> Result<Self> {
        let key = format!("audit_sweep.passed.{target}");
        let mut memory = Self {
            key,
            stamp: stamp.to_string(),
            order: Vec::new(),
            set: Default::default(),
            dirty: false,
        };
        if let Some(raw) = store.kv_get(&memory.key)?
            && let Some((head, pairs)) = raw.split_once('\n')
            && head == memory.stamp
        {
            for memo in pairs
                .split(',')
                .filter_map(|h| u64::from_str_radix(h, 16).ok())
            {
                if memory.set.insert(memo) {
                    memory.order.push(memo);
                }
            }
        } else {
            // No memory yet, or one a different judge or bar wrote: start
            // over, and say so on the next save even if nothing is added.
            memory.dirty = true;
        }
        Ok(memory)
    }

    pub(super) fn contains(&self, memo: u64) -> bool {
        self.set.contains(&memo)
    }

    pub(super) fn insert(&mut self, memo: u64) {
        if self.set.insert(memo) {
            self.order.push(memo);
            self.dirty = true;
        }
    }

    pub(super) fn save(&mut self, store: &dyn Store) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if self.order.len() > SWEEP_MEMORY_MAX {
            let drop = self.order.len() - SWEEP_MEMORY_MAX;
            for memo in self.order.drain(..drop) {
                self.set.remove(&memo);
            }
        }
        let pairs: Vec<String> = self.order.iter().map(|m| format!("{m:x}")).collect();
        store.kv_set(&self.key, &format!("{}\n{}", self.stamp, pairs.join(",")))?;
        self.dirty = false;
        Ok(())
    }
}

/// One unordered pair as a sweep read it: both ids with their content, in id
/// order, and the read (`"claim"`, or the title guard's path) — a pair that
/// crosses the similarity floor is read differently and so judged anew.
pub(super) fn pair_memo(a: &Node, b: &Node, read: &str) -> u64 {
    let (a, b) = if a.id <= b.id { (a, b) } else { (b, a) };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &x in bytes.iter().chain(&[0u8]) {
            h ^= u64::from(x);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for n in [a, b] {
        eat(n.id.as_bytes());
        eat(&content_hash(&n.title, n.body.as_deref()).to_le_bytes());
    }
    eat(read.as_bytes());
    h
}

/// FNV-1a over a note's title and body: a sampling order that depends only
/// on content — stable across runs, platforms and Rust versions (std's
/// hasher promises none of that).
pub(super) fn content_hash(title: &str, body: Option<&str>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let bytes = title.bytes().chain([0u8]).chain(body.unwrap_or("").bytes());
    for b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
