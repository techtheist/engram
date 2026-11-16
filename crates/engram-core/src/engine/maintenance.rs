//! The validation pass (decay, supersession, conflict scan, drift), versions and code refs.

use super::*;

impl Engine {
    /// Path-shaped code_refs that don't resolve against the repo root right
    /// now — the write-time half of the drift check, so the writer learns in
    /// the same turn instead of at the next drift scan.
    pub(super) fn missing_refs(&self, refs: &[String]) -> Vec<String> {
        let Some(root) = &self.repo_root else {
            return Vec::new();
        };
        refs.iter()
            .filter(|r| ref_is_path(r) && !root.join(r.as_str()).exists())
            .cloned()
            .collect()
    }

    /// The graph's current working version (version tracking, 0.7.0).
    pub fn current_version(&self) -> Result<Option<String>> {
        self.store.current_version()
    }

    /// Set (or clear) the current working version — the version every new
    /// node of a version-bound type is stamped with while tracking is on.
    /// Journaled under entity_id "version", so `audit(entity_id="version")`
    /// pages the switch history directly.
    pub fn set_current_version(&self, version: Option<&str>) -> Result<Option<String>> {
        if let Some(v) = version
            && (v.trim().is_empty() || v.len() > 32)
        {
            return Err(crate::Error::Config(
                "version must be 1..=32 non-blank characters".into(),
            ));
        }
        let previous = self.store.current_version()?;
        // Setting a version IS the "I want version tracking" gesture: when
        // tracking is off the stamp would be silently ignored, so enable it
        // here rather than confuse a caller who was asked to set the
        // version. Clearing never toggles anything.
        if version.is_some() && !self.store.config().versioning.enabled {
            let mut cfg = self.graph_config();
            cfg.versioning.enabled = true;
            self.set_graph_config(&cfg)?;
        }
        self.store.set_current_version(version)?;
        self.audit(
            "version_switched",
            "graph",
            "version",
            Some(format!(
                "{} → {}",
                previous.as_deref().unwrap_or("(unset)"),
                version.unwrap_or("(unset)")
            )),
            None,
            None,
            None,
        )?;
        self.notify(ChangeEvent::ConfigChanged);
        // Version rules on tasks ("close once past 0.9.12") fire now.
        self.settle_tasks()?;
        Ok(previous)
    }

    /// One full graph-health pass — the session-boundary validation: the
    /// decay pass archives what has expired, the conflict scan queues fresh
    /// look-alike pairs, and the drift scan counts unresolved code_refs, so
    /// a session starts (and leaves) with the graph prepared rather than
    /// waiting for the six-hourly sweep. Journaled as a `graph_validated`
    /// activity row; returns the summary note.
    pub fn validate_graph(&self) -> Result<String> {
        let prelude = self.validate_prelude()?;
        let suspects = self.scan_conflicts()?;
        self.validate_finish(prelude, suspects)
    }

    /// [`Engine::validate_graph`] for an engine shared behind a mutex — the
    /// shape every long-running caller has (the session-boundary pass, the
    /// core's six-hourly sweep). The cheap parts run under one lock; the
    /// conflict scan — the part whose cost grows with the graph and the
    /// logic model (~60 s at 580 notes with Laya) — releases the lock after
    /// every node, so tool calls and pane requests interleave instead of
    /// queueing behind the sweep. Holding it for the whole scan starved the
    /// core's async workers until even `/health` went silent and every
    /// bridge exited with "the engram core went away" (found 2026-09-24).
    pub fn validate_graph_shared(engine: &std::sync::Mutex<Engine>) -> Result<String> {
        let prelude = engine.locked().validate_prelude()?;
        let suspects = Engine::scan_conflicts_shared(engine)?;
        engine.locked().validate_finish(prelude, suspects)
    }

    /// [`Engine::scan_conflicts`] one node per lock — see
    /// [`Engine::validate_graph_shared`]. A node archived or deleted between
    /// two steps is skipped; a node written meanwhile was already scanned by
    /// its own write.
    pub fn scan_conflicts_shared(engine: &std::sync::Mutex<Engine>) -> Result<usize> {
        let ids = engine.locked().scan_ids()?;
        let mut added = 0;
        for id in ids {
            added += engine.locked().scan_one(&id)?;
            // Releasing is not enough: std's Mutex is not fair, and a loop
            // that unlocks and relocks at once wins every race — a waiting
            // caller measured 12 s behind a 12.5 s sweep. A millisecond
            // off the lock per node hands it over (~0.6 s at 580 notes).
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if added > 0 {
            engine.locked().notify(ChangeEvent::SuspectsChanged);
        }
        Ok(added)
    }

    /// Decay, supersession healing and calibration — the part of a
    /// validation pass that runs before the conflict scan.
    fn validate_prelude(&self) -> Result<(usize, usize, Option<String>)> {
        self.last_validated
            .store(crate::store::now(), std::sync::atomic::Ordering::Relaxed);
        let ttl = self.store.config().policy.decay_ttl_days;
        let archived = self.decay(ttl, false)?.len();
        let retired = self.retire_superseded_sweep()?;
        // Calibrate before scanning, so this session's conflict pass already
        // runs under the floor the graph's own judgments support.
        let tuned = self.auto_tune()?;
        Ok((archived, retired, tuned))
    }

    /// Drift count and the journaled summary — the part after the scan.
    fn validate_finish(
        &self,
        (archived, retired, tuned): (usize, usize, Option<String>),
        suspects: usize,
    ) -> Result<String> {
        let drift = match self.repo_root().map(std::path::Path::to_path_buf) {
            Some(root) => self.scan_code_refs(&root)?.len(),
            None => 0,
        };
        let note = format!(
            "{archived} decayed, {retired} superseded, {suspects} new suspect{}, {drift} drifted ref{}{}",
            if suspects == 1 { "" } else { "s" },
            if drift == 1 { "" } else { "s" },
            match &tuned {
                Some(t) => format!(", {t}"),
                None => String::new(),
            },
        );
        self.audit_activity("graph_validated", Some(note.clone()))?;
        Ok(note)
    }

    /// Whether a fresh [`Engine::validate_graph`] run is due — false within
    /// `min_interval_secs` of the last one on THIS graph.
    pub fn validation_due(&self, min_interval_secs: i64) -> bool {
        let last = self
            .last_validated
            .load(std::sync::atomic::Ordering::Relaxed);
        crate::store::now() - last >= min_interval_secs
    }

    /// Per-graph calibration — one button, several dials (user decision
    /// 2026-08-03: `policy.auto_tune` stays the single switch as calibration
    /// grows parameters). Each dial fits one policy value from evidence this
    /// graph itself produced, applies only a move of at least
    /// [`policy::AUTO_TUNE_MIN_DELTA`], and every move lands in one
    /// `auto_tuned` activity row. `policy.auto_tune = false` opts a graph
    /// out entirely.
    ///
    /// Dial one, the conflict floor — the automated form of the manual
    /// 0.85→0.88 retune (2026-07-13). Every resolved suspect is a labeled
    /// (similarity, verdict) pair: a dismissal is a false positive the floor
    /// exists to prevent, a confirmation is what it must never lose. Models
    /// nominate, people judge — and what people judged is calibration data,
    /// so this fits FROM human verdicts only. Engages over
    /// [`policy::AUTO_TUNE_MIN_NOTES`] current notes with at least
    /// [`policy::AUTO_TUNE_MIN_JUDGED`] judgments,
    /// [`policy::AUTO_TUNE_MIN_EACH`] per side.
    ///
    /// Dial two, the weak line — fits `weak_evidence_top` as a quantile of
    /// the top scores phantom probes (questions about invented subjects
    /// guaranteed absent from any graph) still reach here. The fixed 0.85
    /// default is only right near 2000 notes; the measured calibrated line
    /// runs 0.56→0.81 from 100 to 2000 (tricks bench, 2026-08-03). Engages
    /// over [`policy::WEAK_LINE_MIN_NOTES`] notes, reranker-gated: the line
    /// is read against the cross-encoder's calibrated scale, so there is
    /// nothing to fit without one.
    pub fn auto_tune(&self) -> Result<Option<String>> {
        use crate::policy::{AUTO_TUNE_MIN_NOTES, WEAK_LINE_MIN_NOTES};
        let mut cfg = self.graph_config();
        if !cfg.policy.auto_tune {
            return Ok(None);
        }
        let notes = self.store.stats()?.nodes;
        let mut moves = Vec::new();
        if notes > AUTO_TUNE_MIN_NOTES
            && let Some(m) = self.fit_conflict_floor(&mut cfg)?
        {
            moves.push(m);
        }
        if notes > WEAK_LINE_MIN_NOTES
            && let Some(m) = self.fit_weak_line(&mut cfg)?
        {
            moves.push(m);
        }
        if moves.is_empty() {
            return Ok(None);
        }
        self.set_graph_config(&cfg)?;
        let note = moves.join("; ");
        self.audit_activity("auto_tuned", Some(note.clone()))?;
        Ok(Some(note))
    }

    /// Nodes whose `code_refs` cover a repo-relative file path — the
    /// file-read match hook's lookup. A ref matches
    /// when it names the file exactly or a directory above it. Only current,
    /// non-stale knowledge surfaces (ambient value must not be ambient
    /// noise), strongest trust first.
    pub fn match_code_refs(&self, path: &str, limit: usize) -> Result<Vec<Node>> {
        let path = path.trim().trim_start_matches("./").trim_end_matches('/');
        if path.is_empty() {
            return Ok(Vec::new());
        }
        let covers = |r: &str| {
            let r = r.trim().trim_start_matches("./").trim_end_matches('/');
            !r.is_empty() && (r == path || path.starts_with(&format!("{r}/")))
        };
        let mut hits: Vec<Node> = self
            .store
            .all_nodes()?
            .into_iter()
            .filter(|n| {
                n.valid_until.is_none() && !n.stale && n.code_refs.iter().any(|r| covers(r))
            })
            .collect();
        hits.sort_by(|a, b| {
            b.trust
                .total_cmp(&a.trust)
                .then((b.created_at, &b.id).cmp(&(a.created_at, &a.id)))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    /// Verified code refs: current nodes whose path-shaped
    /// code_refs no longer exist under `root` have drifted — the code moved
    /// or was deleted and the memory didn't follow. A contradiction between
    /// the graph and reality, surfaced for review like a conflict. Reporting
    /// only — drift deliberately does NOT demote: the scan runs on every pane
    /// load against an environment-dependent root (a wrong cwd or a feature
    /// branch with files temporarily gone would mass-stamp sticky demotions
    /// across the graph). Judged conflicts are the demotion trigger; drift is
    /// a review queue. Free-text responsibility labels (anything with
    /// whitespace) are not checkable and never drift.
    pub fn scan_code_refs(&self, root: &std::path::Path) -> Result<Vec<Drift>> {
        let mut out = Vec::new();
        for node in self.store.all_nodes()? {
            if node.valid_until.is_some() || node.code_refs.is_empty() {
                continue;
            }
            let missing: Vec<String> = node
                .code_refs
                .iter()
                .filter(|r| ref_is_path(r) && !root.join(r.as_str()).exists())
                .cloned()
                .collect();
            if !missing.is_empty() {
                out.push(Drift {
                    id: node.id,
                    node_type: node.node_type,
                    title: node.title,
                    missing,
                });
            }
        }
        Ok(out)
    }

    /// The decay pass: archive Claude-authored, never-approved
    /// episodic/volatile nodes that have sat below the stale threshold for
    /// `ttl_days`. Dry-run reports without mutating.
    pub fn decay(&self, ttl_days: i64, dry_run: bool) -> Result<Vec<String>> {
        let now = crate::store::now();
        let candidates = self.store.decay_candidates(ttl_days * 24 * 60 * 60, now)?;
        let ids: Vec<String> = candidates.iter().map(|n| n.id.clone()).collect();
        if dry_run || ids.is_empty() {
            return Ok(ids);
        }
        self.store.archive_nodes(&ids, now)?;
        for candidate in &candidates {
            if let Some(node) = self.store.get_node(&candidate.id)? {
                self.audit_node("archived", Some(candidate), Some(&node))?;
                self.notify(ChangeEvent::NodeUpdated(node));
            }
        }
        Ok(ids)
    }
}

/// A code_ref reads as a checkable path when it has no whitespace and looks
/// filesystem-shaped (a separator or an extension dot); "auth flow"-style
/// responsibility labels fail this and are skipped by the drift scan.
fn ref_is_path(r: &str) -> bool {
    !r.is_empty() && !r.contains(char::is_whitespace) && (r.contains('/') || r.contains('.'))
}
