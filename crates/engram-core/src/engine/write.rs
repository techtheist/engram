//! The checked write path: duplicate match, canon check and suspect queueing.

use super::*;

impl Engine {
    /// Duplicate-guarded note write: a same-type current node at/above the
    /// duplicate threshold is returned instead (the caller merges). Created notes
    /// carry `write_warnings`. Engines behind a mutex use
    /// [`Engine::add_node_checked_shared`].
    pub fn add_node_checked(&self, n: NewNode) -> Result<WriteOutcome> {
        let nli = self.nli.clone();
        match self.plan_add(n, None)? {
            WritePlan::Matched(m) => Ok(m.judge(nli.as_deref())),
            WritePlan::Created(p) => {
                let judged = p.judge(nli.as_deref(), &self.config());
                self.finish_created(judged)
            }
        }
    }

    /// [`Engine::add_node_checked`] over a shared engine: gather under one lock,
    /// judge with the lock released (a write judges up to ~14 pairs; with a heavy
    /// judge that is seconds), queue under a second short lock. `origin` is
    /// re-stamped under each lock since another front-end may stamp in between.
    pub fn add_node_checked_shared(
        engine: &std::sync::Mutex<Engine>,
        origin: AuditOrigin,
        n: NewNode,
    ) -> Result<WriteOutcome> {
        // Embedding off the lock: a moment under it to learn the
        // model and the graph's config, the vectors computed outside it.
        let cache = {
            let (embedder, model, cfg) = {
                let e = engine.locked();
                (e.embedder.clone(), e.embed_model_id(), e.config())
            };
            EmbedCache::for_add(embedder.as_ref(), model, &cfg, &n)
        };
        let (nli, cfg, plan) = {
            let mut e = engine.locked();
            e.set_audit_origin(origin.clone());
            (e.nli.clone(), e.config(), e.plan_add(n, cache.as_ref())?)
        };
        match plan {
            WritePlan::Matched(m) => Ok(m.judge(nli.as_deref())),
            WritePlan::Created(p) => {
                let judged = p.judge(nli.as_deref(), &cfg);
                let mut e = engine.locked();
                e.set_audit_origin(origin);
                e.finish_created(judged)
            }
        }
    }

    /// The locked first half of a checked write: embed (or take the vectors
    /// `cache` computed before the lock), short-circuit to a near-duplicate,
    /// or create and gather everything the judge will read.
    fn plan_add(&self, n: NewNode, cache: Option<&EmbedCache>) -> Result<WritePlan> {
        let scrubbed_title = crate::redact::scrub(&n.title);
        let scrubbed_body = n.body.as_deref().map(crate::redact::scrub);
        let vec = self.embed_one_with(
            &embed_text(
                &scrubbed_title,
                scrubbed_body.as_deref(),
                &n.tags,
                &n.code_refs,
            ),
            cache,
        )?;

        let duplicate_similarity = self.store.config().policy.duplicate_similarity;
        for (id, distance) in self.store.search_vec(&vec, WRITE_CHECK_K)? {
            let similarity = 1.0 - distance;
            if similarity < duplicate_similarity {
                break; // results are distance-ordered; nothing closer follows
            }
            if let Some(node) = self.store.get_node(&id)?
                && node.node_type == n.node_type
                && node.valid_until.is_none()
                // Titles that name different things are no duplicate whatever the vectors
                // say: shared boilerplate bodies made unrelated Cautions match at 0.90 and
                // hid resurrections. Undecidable titles keep matching.
                && !subjects_differ(&scrubbed_title, &node.title)
            {
                let text = match &scrubbed_body {
                    Some(b) => format!("{scrubbed_title}. {b}"),
                    None => scrubbed_title.clone(),
                };
                return Ok(WritePlan::Matched(Box::new(MatchedPlan {
                    node,
                    similarity,
                    excerpt: text.chars().take(400).collect(),
                })));
            }
        }

        let missing_refs = self.missing_refs(&n.code_refs);
        let node = self.add_node_opts(n, true, cache)?;
        Ok(WritePlan::Created(Box::new(self.plan_created(
            node,
            vec,
            missing_refs,
        )?)))
    }

    /// What a fresh text's checks need from the store, read under the lock:
    /// the warnings (store-only, final here) and the pairs to judge.
    fn plan_created(
        &self,
        node: Node,
        vec: Vec<f32>,
        missing_refs: Vec<String>,
    ) -> Result<CreatedPlan> {
        let near = self.nearby(&vec, &node.id)?;
        let warnings = self.write_warnings(&near, &node.id, &node.title)?;
        let suspects = self.suspect_candidates(&node, &near)?;
        let canon = self.canon_candidates(&near)?;
        let bridges = self.bridge_candidates(&node)?;
        Ok(CreatedPlan {
            node,
            warnings,
            missing_refs,
            suspects,
            canon,
            bridges,
        })
    }

    /// The locked last half of a checked write: queue what the judge
    /// nominated and assemble the verdict.
    fn finish_created(&self, j: JudgedPlan) -> Result<WriteOutcome> {
        let mut suspects = self.queue_judged(&j.node, j.suspects)?;
        self.queue_judged_bridges(j.bridges, &mut suspects)?;
        Ok(WriteOutcome::Created {
            node: j.node,
            warnings: j.warnings,
            suspects,
            missing_refs: j.missing_refs,
            canon: j.canon,
        })
    }

    /// Queue judged suspects near `node`; returns their judgeable views.
    fn queue_judged(&self, node: &Node, judged: Vec<JudgedSuspect>) -> Result<Vec<SuspectView>> {
        if self.queue_suspects(node, judged)? == 0 {
            return Ok(Vec::new());
        }
        self.notify(ChangeEvent::SuspectsChanged);
        self.suspects_involving(&node.id)
    }

    /// The nearest current neighbourhood of a fresh text, read once for the
    /// warnings, suspect and canon checks. Distance-ordered, `exclude_id` left out.
    pub(super) fn nearby(&self, vec: &[f32], exclude_id: &str) -> Result<Vec<(Node, f64)>> {
        let mut out = Vec::new();
        for (id, distance) in self.store.search_vec(vec, WRITE_CHECK_K)? {
            if id == exclude_id {
                continue;
            }
            if let Some(node) = self.store.get_node(&id)? {
                out.push((node, 1.0 - distance));
            }
        }
        Ok(out)
    }

    /// The write-time canon check's candidates: the nearest
    /// current knowledge above the warn line, capped — judged by
    /// [`judge_canon`]. Empty without the logic layer.
    fn canon_candidates(&self, near: &[(Node, f64)]) -> Result<Vec<(Node, f64)>> {
        if self.nli.is_none() {
            return Ok(Vec::new());
        }
        let cfg = self.store.config();
        let mut out = Vec::new();
        for (node, similarity) in near {
            let (node, similarity) = (node.clone(), *similarity);
            if similarity < cfg.policy.warn_similarity {
                break; // distance-ordered: nothing closer follows
            }
            if out.len() >= CANON_CHECK_CAP {
                break;
            }
            // Tombstones are skipped here on purpose: "Removed: X" against
            // "X" is not a pair the NLI reads reliably, and the write already
            // carries a `tombstoned` warning for the same neighbor — one
            // channel per fact, no double report.
            if node.valid_until.is_some() || is_anchor(&cfg, &node) || is_tombstone(&cfg, &node) {
                continue;
            }
            out.push((node, similarity));
        }
        Ok(out)
    }

    /// `update_node` plus conflict warnings and freshly-queued suspects when
    /// any embedded field changed. Shared front-ends call
    /// [`Engine::update_node_checked_shared`].
    pub fn update_node_checked(&self, id: &str, patch: NodePatch) -> Result<CheckedUpdate> {
        let nli = self.nli.clone();
        let judged = self
            .plan_update(id, patch, None)?
            .judge(nli.as_deref(), &self.config());
        self.finish_update(judged)
    }

    /// [`Engine::update_node_checked`] judged with the lock released — see
    /// [`Engine::add_node_checked_shared`].
    pub fn update_node_checked_shared(
        engine: &std::sync::Mutex<Engine>,
        origin: AuditOrigin,
        id: &str,
        patch: NodePatch,
    ) -> Result<CheckedUpdate> {
        // Embedding off the lock, as on add: the texts are predicted from
        // the stored node as the patch will leave it; if the node changes
        // in between, the misses are embedded under the lock.
        let cache = {
            let (embedder, model, cfg, before) = {
                let e = engine.locked();
                (
                    e.embedder.clone(),
                    e.embed_model_id(),
                    e.config(),
                    e.store.get_node(id)?,
                )
            };
            before.and_then(|b| EmbedCache::for_update(embedder.as_ref(), model, &cfg, &b, &patch))
        };
        let (nli, cfg, plan) = {
            let mut e = engine.locked();
            e.set_audit_origin(origin.clone());
            (
                e.nli.clone(),
                e.config(),
                e.plan_update(id, patch, cache.as_ref())?,
            )
        };
        let judged = plan.judge(nli.as_deref(), &cfg);
        let mut e = engine.locked();
        e.set_audit_origin(origin);
        e.finish_update(judged)
    }

    /// The locked first half of a checked update. An update that touches no
    /// embedded field has nothing to check: its plan carries no pairs.
    fn plan_update(
        &self,
        id: &str,
        patch: NodePatch,
        cache: Option<&EmbedCache>,
    ) -> Result<CreatedPlan> {
        let touches_text = patch.title.is_some()
            || patch.body.is_some()
            || patch.tags.is_some()
            || patch.code_refs.is_some();
        let node = self.update_node_with(id, patch, cache)?;
        let missing_refs = self.missing_refs(&node.code_refs);
        if !touches_text {
            return Ok(CreatedPlan {
                node,
                warnings: Vec::new(),
                missing_refs,
                suspects: Vec::new(),
                canon: Vec::new(),
                bridges: Vec::new(),
            });
        }
        let vec = self.embed_one_with(
            &embed_text(
                &node.title,
                node.body.as_deref(),
                &node.tags,
                &node.code_refs,
            ),
            cache,
        )?;
        self.plan_created(node, vec, missing_refs)
    }

    fn finish_update(&self, j: JudgedPlan) -> Result<CheckedUpdate> {
        let mut suspects = self.queue_judged(&j.node, j.suspects)?;
        self.queue_judged_bridges(j.bridges, &mut suspects)?;
        Ok(CheckedUpdate {
            node: j.node,
            warnings: j.warnings,
            suspects,
            missing_refs: j.missing_refs,
            canon: j.canon,
        })
    }

    /// Pending suspects that involve this node — the judgeable form of what a
    /// write just queued.
    fn suspects_involving(&self, node_id: &str) -> Result<Vec<SuspectView>> {
        Ok(self
            .store
            .suspects_pending()?
            .into_iter()
            .filter(|s| s.a.id == node_id || s.b.id == node_id)
            .collect())
    }

    /// Nearby nodes that are contradicted, superseded or tombstoned, so the writer
    /// notices contested or deliberately removed ground. Superseded means "follow
    /// the successor"; tombstoned means "removed without one, re-adding is the
    /// error". The tombstone check is cross-type and has two channels: the vector
    /// neighbourhood (kept 8 deep; 24 deep misfired on 38% of rewrites) and an
    /// exact victim-title match (resurrection warned 0.81 → 1.00).
    fn write_warnings(
        &self,
        near: &[(Node, f64)],
        exclude_id: &str,
        title: &str,
    ) -> Result<Vec<WriteWarning>> {
        let mut warnings = Vec::new();
        let cfg = self.store.config();
        let warn_similarity = cfg.policy.warn_similarity;
        for (node, similarity) in near {
            let (node, similarity) = (node.clone(), *similarity);
            if similarity < warn_similarity {
                break;
            }
            let (reason, note) = if node.valid_until.is_some() {
                ("superseded", None)
            } else if is_tombstone(&cfg, &node) {
                ("tombstoned", tombstone_note(&node))
            } else if self.store.has_active_conflict(&node.id)? {
                ("in-active-conflict", None)
            } else {
                continue;
            };
            warnings.push(WriteWarning {
                id: node.id,
                title: node.title,
                reason: reason.to_string(),
                similarity,
                note,
            });
        }
        if cfg.tombstone_type().is_some() && !warnings.iter().any(|w| w.reason == "tombstoned") {
            let wanted = normalized_title(title);
            if !wanted.is_empty() {
                for node in self.store.all_nodes()? {
                    if node.id == exclude_id
                        || node.valid_until.is_some()
                        || !is_tombstone(&cfg, &node)
                        || normalized_title(tombstone_victim_title(&node.title)) != wanted
                    {
                        continue;
                    }
                    warnings.push(WriteWarning {
                        note: tombstone_note(&node),
                        id: node.id,
                        title: node.title,
                        reason: "tombstoned".to_string(),
                        similarity: 1.0,
                    });
                    break;
                }
            }
        }
        Ok(warnings)
    }
}

/// A logic-layer triage hint: label, score, and — for contradictions — the
/// side carrying the negation (see [`Engine::nli_hint`]).
pub type Hint = (&'static str, f64, Option<&'static str>);

/// How many nearest neighbours the write-time canon check judges.
const CANON_CHECK_CAP: usize = 5;
const CANON_SUPPORT: f32 = 0.6;
const CANON_CONTRADICTION: f32 = 0.7;

/// A checked write's locked first half: a near-duplicate to hand back, or a
/// created note with the pairs its checks must judge.
enum WritePlan {
    Matched(Box<MatchedPlan>),
    Created(Box<CreatedPlan>),
}

struct MatchedPlan {
    pub(super) node: Node,
    pub(super) similarity: f64,
    /// The new text as the judge reads it against the match.
    pub(super) excerpt: String,
}

impl MatchedPlan {
    /// At duplicate similarity co-reference holds, so an NLI contradiction
    /// is trustworthy — it flags the negated near-duplicate a cosine score
    /// can't see.
    pub(super) fn judge(self, nli: Option<&dyn Nli>) -> WriteOutcome {
        let (nli_label, nli_score) =
            match nli.and_then(|n| n.judge_pair(&self.excerpt, &claim(&self.node)).ok()) {
                Some(sym) => {
                    let (l, s) = sym.hint();
                    (Some(l.to_string()), Some(f64::from(s)))
                }
                None => (None, None),
            };
        WriteOutcome::Matched {
            node: self.node,
            similarity: self.similarity,
            nli_label,
            nli_score,
        }
    }
}

/// A written note (created or updated) plus everything its checks read from
/// the store — gathered under the engine lock, judged without it.
struct CreatedPlan {
    pub(super) node: Node,
    pub(super) warnings: Vec<WriteWarning>,
    pub(super) missing_refs: Vec<String>,
    pub(super) suspects: Vec<SuspectCandidate>,
    pub(super) canon: Vec<(Node, f64)>,
    /// Bridged pairs: facts about two subjects a bridge ties.
    pub(super) bridges: Vec<BridgeCandidate>,
}

impl CreatedPlan {
    /// The lock-free half: every logic-layer call a write makes.
    pub(super) fn judge(
        self,
        nli: Option<&dyn Nli>,
        cfg: &crate::config::GraphConfig,
    ) -> JudgedPlan {
        let suspects = judge_suspects(nli, cfg, &self.node, self.suspects);
        let canon = judge_canon(
            nli,
            cfg.policy.conflict_suspect_similarity,
            &claim(&self.node),
            self.canon,
        );
        let bridges = judge_bridges(nli, cfg.policy.conflict_bridge_gate, self.bridges);
        JudgedPlan {
            node: self.node,
            warnings: self.warnings,
            missing_refs: self.missing_refs,
            suspects,
            canon,
            bridges,
        }
    }
}

struct JudgedPlan {
    pub(super) node: Node,
    pub(super) warnings: Vec<WriteWarning>,
    pub(super) missing_refs: Vec<String>,
    pub(super) suspects: Vec<JudgedSuspect>,
    pub(super) canon: Vec<CanonVerdict>,
    pub(super) bridges: Vec<(BridgeCandidate, f64)>,
}

/// One pair the conflict scan will judge. `title_path` is set for a
/// below-floor pair the subject guard admitted ("named" | "unnamed"): it
/// queues only on a confident title contradiction.
pub(super) struct SuspectCandidate {
    pub(super) other: Node,
    pub(super) similarity: f64,
    pub(super) title_path: Option<&'static str>,
}

/// A judged candidate that queues, with its hint.
pub(super) type JudgedSuspect = (SuspectCandidate, Option<Hint>);

/// Judge suspect candidates — no engine, no lock. A look-alike always
/// queues, hinted when the judge is loaded; a below-floor pair queues only
/// on a title contradiction at or above the gate.
pub(super) fn judge_suspects(
    nli: Option<&dyn Nli>,
    cfg: &crate::config::GraphConfig,
    node: &Node,
    candidates: Vec<SuspectCandidate>,
) -> Vec<JudgedSuspect> {
    let gate = cfg.policy.conflict_nli_gate;
    candidates
        .into_iter()
        .filter_map(|c| {
            let hint = match c.title_path {
                None => nli.and_then(|n| claim_hint(n, node, &c.other)),
                Some(path) => {
                    let (nli, gate) = (nli?, gate?);
                    match title_hint_for(nli, path, node, &c.other) {
                        Some(h) if h.0 == "contradiction" && h.1 >= gate => Some(h),
                        _ => return None,
                    }
                }
            };
            Some((c, hint))
        })
        .collect()
}

/// Write-time canon check against the nearest knowledge. `supports` is cheap to
/// trust; `contradicts` only inside the suspect band, where co-reference holds.
fn judge_canon(
    nli: Option<&dyn Nli>,
    suspect_similarity: f64,
    text: &str,
    candidates: Vec<(Node, f64)>,
) -> Vec<CanonVerdict> {
    let Some(nli) = nli else {
        return Vec::new();
    };
    let excerpt: String = text.chars().take(400).collect();
    let mut out = Vec::new();
    for (node, similarity) in candidates {
        let Ok(j) = nli.judge_pair(&claim(&node), &excerpt) else {
            continue;
        };
        let verdict =
            if j.contradiction() >= CANON_CONTRADICTION && similarity >= suspect_similarity {
                Some(("contradicts", j.contradiction()))
            } else if j.forward.entailment >= CANON_SUPPORT {
                Some(("supports", j.forward.entailment))
            } else {
                None
            };
        if let Some((verdict, score)) = verdict {
            out.push(CanonVerdict {
                id: node.id,
                node_type: node.node_type,
                title: node.title,
                verdict: verdict.into(),
                score: f64::from(score),
                similarity,
            });
        }
    }
    // Contradictions first — they are the act-now verdicts.
    out.sort_by(|a, b| {
        (b.verdict == "contradicts")
            .cmp(&(a.verdict == "contradicts"))
            .then(b.score.total_cmp(&a.score))
    });
    out
}

/// [`Engine::nli_hint`] without the engine.
pub(super) fn claim_hint(nli: &dyn Nli, a: &Node, b: &Node) -> Option<Hint> {
    let sym = nli.judge_pair(&claim(a), &claim(b)).ok()?;
    Some(hint_with_direction(&sym, a, b))
}

/// [`Engine::nli_title_hint`] without the engine.
pub(super) fn title_hint(nli: &dyn Nli, a: &Node, b: &Node) -> Option<Hint> {
    let sym = nli.judge_pair(a.title.trim(), b.title.trim()).ok()?;
    Some(hint_with_direction(&sym, a, b))
}

/// [`Engine::nli_title_hint_clause`] without the engine.
pub(super) fn title_hint_clause(nli: &dyn Nli, a: &Node, b: &Node) -> Option<Hint> {
    let (ta, tb) = (a.title.trim(), b.title.trim());
    let mut sym = nli.judge_pair(ta, tb).ok()?;
    let (ca, cb) = (title_clause(ta), title_clause(tb));
    if (ca, cb) != (ta, tb)
        && let Ok(clause) = nli.judge_pair(ca, cb)
        && clause.contradiction() > sym.contradiction()
    {
        sym = clause;
    }
    Some(hint_with_direction(&sym, a, b))
}

/// [`Engine::nli_title_hint_for`] without the engine.
pub(super) fn title_hint_for(nli: &dyn Nli, path: &str, a: &Node, b: &Node) -> Option<Hint> {
    if path == "unnamed" {
        title_hint_clause(nli, a, b)
    } else {
        title_hint(nli, a, b)
    }
}
