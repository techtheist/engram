//! The suspected-conflict queue: nomination, NLI hints, check_claim and resolution.

use super::*;

impl Engine {
    /// Verify a claim against canon: judge the nearest nodes (node as premise,
    /// claim as hypothesis) into supports / contradicts / silent. "The canon
    /// disagrees" and "the canon doesn't know" are different answers.
    pub fn check_claim(&self, text: &str, limit: usize) -> Result<ClaimReport> {
        let Some(nli) = &self.nli else {
            return Err(crate::Error::Embedding(
                "the NLI model is not loaded — claim checks need the local logic layer".into(),
            ));
        };
        let qv = self.embedder.embed_one(text)?;
        // Unwindowed on purpose: a claim is checked against the whole canon,
        // because knowledge that contradicts it does not stop counting for
        // being old.
        let hits = self.store.search_hybrid(
            text,
            Some(&qv),
            &[],
            limit.clamp(4, 16),
            Default::default(),
            &Default::default(),
        )?;
        let cfg = self.config();
        let mut report = ClaimReport {
            claim: text.to_string(),
            supports: Vec::new(),
            contradicts: Vec::new(),
            retracted: Vec::new(),
            silent: Vec::new(),
        };
        let mut nodes = Vec::new();
        for h in &hits {
            let Some(n) = self.store.get_node(&h.id)? else {
                continue;
            };
            // A tombstone-role hit is sorted by role before any model runs: the claim
            // lands on deliberately removed knowledge ("retracted"). The retrieval score
            // rides in `entailment` so the bucket still sorts strongest first.
            if is_tombstone(&cfg, &n) {
                report.retracted.push(ClaimVerdict {
                    id: n.id,
                    node_type: n.node_type,
                    title: n.title,
                    trust: n.trust,
                    stale: n.stale,
                    entailment: h.score as f32,
                    neutral: 0.0,
                    contradiction: 0.0,
                    project: None,
                });
                continue;
            }
            nodes.push(n);
        }
        let pairs: Vec<(String, String)> =
            nodes.iter().map(|n| (claim(n), text.to_string())).collect();
        let judgments = nli.judge(&pairs)?;

        for (node, j) in nodes.into_iter().zip(judgments) {
            let verdict = ClaimVerdict {
                id: node.id,
                node_type: node.node_type,
                title: node.title,
                trust: node.trust,
                stale: node.stale,
                entailment: j.entailment,
                neutral: j.neutral,
                contradiction: j.contradiction,
                project: None,
            };
            // An unconfident contradiction is reported as silence: every assertion costs
            // a human judgment, and ungated the model calls most unrelated claims
            // contradictions (`eval/CONTRADICTIONS.md`). Contradiction-only: false
            // `supports` was never measured.
            let floor = self.config().policy.claim_contradiction_min_confidence;
            match j.label() {
                "entailment" => report.supports.push(verdict),
                "contradiction" if f64::from(verdict.contradiction) >= floor => {
                    report.contradicts.push(verdict);
                }
                _ => report.silent.push(verdict),
            }
        }
        report
            .contradicts
            .sort_by(|a, b| b.contradiction.total_cmp(&a.contradiction));
        // Strongest near-miss first: this bucket collects what the layer declined to
        // assert, including verbatim restatements judged neutral.
        report.silent.sort_by(|a, b| {
            b.entailment
                .max(b.contradiction)
                .total_cmp(&a.entailment.max(a.contradiction))
        });
        report
            .supports
            .sort_by(|a, b| b.entailment.total_cmp(&a.entailment));
        report
            .retracted
            .sort_by(|a, b| b.entailment.total_cmp(&a.entailment));
        Ok(report)
    }

    /// Shared candidate logic: nearest neighbors above the suspect threshold,
    /// both active and non-anchor, not already linked by any edge, pair never
    /// raised before. Stored newer-first so `replaces` verdicts read forward.
    pub(super) fn suspects_near(&self, node: &Node, vec: &[f32]) -> Result<usize> {
        let candidates = self.suspect_candidates(node, &self.nearby(vec, &node.id)?)?;
        let judged = judge_suspects(self.nli.as_deref(), &self.config(), node, candidates);
        self.queue_suspects(node, judged)
    }

    /// The pairs [`Engine::suspects_near`] judges — store reads only, so a
    /// shared write gathers them under the lock and judges them off it.
    pub(super) fn suspect_candidates(
        &self,
        node: &Node,
        near: &[(Node, f64)],
    ) -> Result<Vec<SuspectCandidate>> {
        let cfg = self.store.config();
        // Tombstones sit out both sides, as in the sweep: one resembles its victim by
        // design, and a `replaces` verdict would archive the marker.
        if is_anchor(&cfg, node) || is_tombstone(&cfg, node) || node.valid_until.is_some() {
            return Ok(Vec::new());
        }
        // Two ways in: above the similarity floor a look-alike queues on similarity
        // alone; below it (down to the NLI floor) NLI reads the bare titles, and a
        // confident contradiction between same-subject titles queues too. Similarity
        // can't tell contradiction from restatement (AUROC 0.59).
        let nli_gate = cfg.policy.conflict_nli_gate.filter(|_| self.nli.is_some());
        let floor = match nli_gate {
            Some(_) => crate::policy::CONFLICT_NLI_FLOOR,
            None => cfg.policy.conflict_suspect_similarity,
        };
        let mut out = Vec::new();
        for (other, similarity) in near {
            let (other, similarity) = (other.clone(), *similarity);
            if similarity < floor {
                break; // distance-ordered: nothing closer follows
            }
            if self.pair_blocked(node, &other)? {
                continue;
            }
            let title_path = if similarity >= cfg.policy.conflict_suspect_similarity {
                None
            } else {
                if nli_gate.is_none() {
                    continue;
                }
                let Some(path) = title_pair_admission(&node.title, &other.title) else {
                    continue;
                };
                Some(path)
            };
            out.push(SuspectCandidate {
                other,
                similarity,
                title_path,
            });
        }
        Ok(out)
    }

    /// Whether a pair may not queue: the other side is an anchor, a tombstone
    /// or archived, or the two are already linked or already suspected.
    pub(super) fn pair_blocked(&self, node: &Node, other: &Node) -> Result<bool> {
        let cfg = self.store.config();
        Ok(is_anchor(&cfg, other)
            || is_tombstone(&cfg, other)
            || other.valid_until.is_some()
            || self.store.pair_linked(&node.id, &other.id)?
            || self.store.suspect_between(&node.id, &other.id)?)
    }

    /// Queue judged pairs, each re-checked first: on the shared path the lock
    /// was free while the judge ran, so either side may have been archived,
    /// deleted, linked or queued since.
    pub(super) fn queue_suspects(&self, node: &Node, judged: Vec<JudgedSuspect>) -> Result<usize> {
        if judged.is_empty() {
            return Ok(0);
        }
        let Some(node) = self.store.get_node(&node.id)? else {
            return Ok(0);
        };
        if node.valid_until.is_some() {
            return Ok(0);
        }
        let cfg = self.store.config();
        let mut added = 0;
        for (candidate, hint) in judged {
            let Some(other) = self.store.get_node(&candidate.other.id)? else {
                continue;
            };
            if self.pair_blocked(&node, &other)? {
                continue;
            }
            let (newer, older): (&Node, &Node) = if node.created_at >= other.created_at {
                (&node, &other)
            } else {
                (&other, &node)
            };
            self.store
                .add_suspect(&newer.id, &older.id, candidate.similarity, hint)?;
            added += 1;
            // A confident contradiction of the older note reaches whatever
            // inherits from it: the judge read
            // one same-subject pair, the edges carry the rest.
            let inherit_gate = cfg
                .policy
                .conflict_nli_gate
                .unwrap_or(cfg.policy.nli_sweep_min_confidence);
            if let Some(("contradiction", score, _)) = hint
                && score >= inherit_gate
            {
                added += self.propagate_inherited(newer, &older.id, score)?;
            }
        }
        Ok(added)
    }

    /// The NLI triage hint for a pair: a nomination, never a verdict. `None` when
    /// no model is loaded or judging fails. For contradictions the third element
    /// names the side (`"newer"`/`"older"`) carrying the negation. Public so the
    /// eval harness scores the real composition, not a reimplementation.
    pub fn nli_hint(&self, a: &Node, b: &Node) -> Option<Hint> {
        claim_hint(self.nli.as_deref()?, a, b)
    }

    /// [`Engine::nli_hint`] on the bare titles, without the body sentence
    /// `claim` appends: titles separate contradiction from restatement far
    /// more sharply (negation 0.99 vs 0.61).
    pub fn nli_title_hint(&self, a: &Node, b: &Node) -> Option<Hint> {
        title_hint(self.nli.as_deref()?, a, b)
    }

    /// [`Engine::nli_title_hint`] read whole and cut to the first clauses
    /// ([`title_clause`]); the stronger contradiction wins. A trailing clause is
    /// commentary the model weighs against the claim (clause shape 0.04 → 0.58+).
    /// Unnamed admission path only: on named pairs the cut pushed seven
    /// same-subject-different-fact pairs over the gate on the dogfood graphs.
    pub fn nli_title_hint_clause(&self, a: &Node, b: &Node) -> Option<Hint> {
        title_hint_clause(self.nli.as_deref()?, a, b)
    }

    /// The title read the sweep uses for a pair the guard admitted on
    /// `path` ("named" | "unnamed").
    pub fn nli_title_hint_for(&self, path: &str, a: &Node, b: &Node) -> Option<Hint> {
        title_hint_for(self.nli.as_deref()?, path, a, b)
    }

    /// The pending queue, ready for judgment.
    pub fn suspects(&self) -> Result<Vec<SuspectView>> {
        self.store.suspects_pending()
    }

    /// Score the "models nominate, people judge" loop: over every judged
    /// suspect pair, how often the local NLI hint (nli_label) matched the
    /// judge's verdict. Read-only — a report, never a calibration input
    /// (dial one reads similarity, not the hint).
    pub fn nli_agreement(&self) -> Result<NliAgreement> {
        let mut r = NliAgreement::default();
        for s in self.store.all_suspects()? {
            let confirmed = match s.status {
                SuspectStatus::Confirmed => true,
                SuspectStatus::Dismissed => false,
                SuspectStatus::Suspected => continue,
            };
            r.judged += 1;
            let Some(label) = s.nli_label.as_deref() else {
                continue;
            };
            r.with_hint += 1;
            match (label == "contradiction", confirmed) {
                (true, true) => r.hits += 1,
                (true, false) => r.false_alarms += 1,
                (false, true) => r.misses += 1,
                (false, false) => r.passes += 1,
            }
        }
        if r.with_hint > 0 {
            r.agreement = Some((r.hits + r.passes) as f64 / r.with_hint as f64);
        }
        Ok(r)
    }

    /// Judge a suspected pair. `conflict` records a `conflicts-with` edge;
    /// `replaces` records the edge *and* archives the older node (the
    /// supersede-not-delete flow, PLAN §6B); `dismiss` marks the pair judged
    /// so it is never re-raised. Already-judged suspects are a no-op.
    pub fn resolve_suspect(
        &self,
        id: &str,
        verdict: SuspectVerdict,
        source: Source,
    ) -> Result<Option<Edge>> {
        // One operation in the journal: undo reverts all of it.
        let _op = self.op_scope();
        let Some(suspect) = self.store.get_suspect(id)? else {
            return Err(crate::Error::NotFound(id.to_string()));
        };
        if suspect.status != SuspectStatus::Suspected {
            return Ok(None);
        }
        let edge = match verdict {
            SuspectVerdict::Dismiss => None,
            SuspectVerdict::Conflict => Some(self.add_edge(NewEdge {
                edge_type: self.store.config().contradiction_edge(),
                from_id: suspect.a_id.clone(),
                to_id: suspect.b_id.clone(),
                source,
                note: Some("confirmed from conflict scan".into()),
                confidence: Some(suspect.similarity),
                strength: None,
                status: None,
            })?),
            SuspectVerdict::Replaces => {
                // A pin is the user's "never fade" — an assistant verdict
                // must not archive it. Surface instead; the user can still
                // replace it from the pane (a user verdict proceeds).
                if source == Source::Agent
                    && let Some(older) = self.store.get_node(&suspect.b_id)?
                    && older.trust_override.is_some()
                {
                    return Err(crate::Error::Pinned(format!(
                        "\"{}\" ({}) is user-pinned; a replaces verdict would archive it — \
                         tell the user and let them judge this pair in the pane",
                        older.title, older.id
                    )));
                }
                let edge = self.add_edge(NewEdge {
                    edge_type: self.store.config().supersession_edge(),
                    from_id: suspect.a_id.clone(),
                    to_id: suspect.b_id.clone(),
                    source,
                    note: Some("confirmed from conflict scan".into()),
                    confidence: Some(suspect.similarity),
                    strength: None,
                    status: None,
                })?;
                // add_edge already retired the older node — unless it is
                // pinned, where the automatic path steps aside. A USER
                // verdict overrides the pin (the assistant case errored
                // above), so archive it here explicitly.
                if let Some(older) = self.store.get_node(&suspect.b_id)?
                    && older.valid_until.is_none()
                {
                    self.update_node(
                        &suspect.b_id,
                        NodePatch {
                            valid_until: Some(crate::store::now()),
                            ..NodePatch::default()
                        },
                    )?;
                }
                Some(edge)
            }
        };
        let status = match verdict {
            SuspectVerdict::Dismiss => SuspectStatus::Dismissed,
            _ => SuspectStatus::Confirmed,
        };
        self.store.set_suspect_status(id, status)?;
        self.notify(ChangeEvent::SuspectsChanged);
        Ok(edge)
    }
}

/// Label, confidence and, for contradictions, the side (`"newer"`/`"older"`)
/// that contradicts hardest as the hypothesis. No direction under a 0.15
/// asymmetry margin.
pub(super) fn hint_with_direction(
    sym: &crate::nli::SymmetricJudgment,
    a: &Node,
    b: &Node,
) -> (&'static str, f64, Option<&'static str>) {
    const DIRECTION_MARGIN: f32 = 0.15;
    let (label, score) = sym.hint();
    let direction = if label == "contradiction" {
        // forward = (a premise → b hypothesis): high forward contradiction
        // reads b as the negated claim.
        let carrier = if sym.forward.contradiction >= sym.backward.contradiction + DIRECTION_MARGIN
        {
            Some(b)
        } else if sym.backward.contradiction >= sym.forward.contradiction + DIRECTION_MARGIN {
            Some(a)
        } else {
            None
        };
        let a_is_newer = a.created_at >= b.created_at;
        carrier.map(|c| {
            if std::ptr::eq(c, a) == a_is_newer {
                "newer"
            } else {
                "older"
            }
        })
    } else {
        None
    };
    (label, f64::from(score), direction)
}

/// A node's canonical claim for NLI: the title, plus the body's first sentence
/// when it adds context. Claim-level because whole bodies dilute a
/// sentence-pair model.
pub(super) fn claim(node: &Node) -> String {
    let mut text = node.title.trim().to_string();
    if let Some(body) = node.body.as_deref() {
        let first = body
            .trim()
            .replace('\n', " ")
            .split(". ")
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if !first.is_empty() && !text.to_lowercase().contains(&first.to_lowercase()) {
            text.push_str(". ");
            text.push_str(&first);
        }
    }
    text.chars().take(400).collect()
}
