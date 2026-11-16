//! The session-history layer: its store handle, recorded messages, and history search.

use super::*;

impl Engine {
    /// Tell this engine where its history store lives (serve wires it to
    /// `history.tepin` beside the curated store). Opens right away when the
    /// graph's history config is enabled.
    pub fn set_history_path(&mut self, path: std::path::PathBuf) {
        self.history_path = Some(path);
        self.sync_history_store();
    }

    /// Where this graph's history store lives, when serve installed it.
    pub fn history_path(&self) -> Option<&std::path::Path> {
        self.history_path.as_deref()
    }

    /// Run `f` against the history layer's store — `None` when the layer is
    /// disabled, pathless, or failed to open. The closure shape (rather than
    /// a borrow) is what lets config writes swap the handle under `&self`.
    pub fn with_history<R>(&self, f: impl FnOnce(&dyn Store) -> R) -> Option<R> {
        self.history.locked().as_deref().map(f)
    }

    /// Whether the history layer is live on this engine right now.
    pub fn history_open(&self) -> bool {
        self.history.locked().is_some()
    }

    /// Write one node into the history layer and embed it there — the
    /// harvester's write path. Deliberately NOT [`Engine::add_node`]: no
    /// audit row, no change event, no dupe verdicts, no version stamp —
    /// history is records, not knowledge.
    ///
    /// Sealing is the STORE DRIVER's job since 0.9.0 (its recorded state
    /// decides); this path scrubs plaintext and embeds the scrubbed
    /// plaintext — vectors stay deliberately open, the documented inversion
    /// risk. `Ok(None)` when the layer is disabled or closed.
    pub fn add_history_node(&self, mut n: NewNode) -> Result<Option<Node>> {
        n.title = crate::redact::scrub(&n.title);
        n.body = n.body.as_deref().map(crate::redact::scrub);
        let Some(node) = self.with_history(|s| s.add_node(n)).transpose()? else {
            return Ok(None);
        };
        // The driver returned the node it stored, opened for us — plaintext.
        let mut texts = vec![embed_text(
            &node.title,
            node.body.as_deref(),
            &node.tags,
            &node.code_refs,
        )];
        texts.extend(claim_texts(&node.title, node.body.as_deref()));
        let vectors = self.embedder.embed(&texts)?;
        self.with_history(|s| s.upsert_embeddings(&node.id, &vectors))
            .transpose()?;
        Ok(Some(node))
    }

    /// Write one edge into the history layer (`in`/`next`/`born-in` chains).
    /// `Ok(None)` when the layer is disabled or closed.
    pub fn add_history_edge(&self, e: NewEdge) -> Result<Option<Edge>> {
        self.with_history(|s| s.add_edge(e)).transpose()
    }

    /// Search the history layer: hybrid since 0.9.0 — vector candidates
    /// fused with the blind-index BM25 channel (identical sealed or
    /// plaintext; before 0.9.0 no keyword index existed over history at
    /// all, caution 00bgftfbusll), then the cross-encoder re-scores the
    /// candidate texts at query time. Scores live on the reranker's scale
    /// but are NEVER blended with curated scores (the 0.8.1 register
    /// lesson) — the caller renders these as their own labeled section.
    /// Empty when the layer is off or `search_fallthrough` gates it.
    pub fn search_history(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<crate::history::HistoryHit>> {
        self.search_history_filtered(query, limit, &SearchFilter::default())
    }

    /// [`Engine::search_history`] scoped to a time window and ordering
    /// — the same grammar the curated layer reads, against message
    /// timestamps. Recorded dialogue is the layer where "when" is most often
    /// the whole question ("what did we try on Tuesday"), so the window
    /// filters candidates before the cross-encoder re-scores them and before
    /// the section gate decides whether the section exists at all.
    pub fn search_history_filtered(
        &self,
        query: &str,
        limit: usize,
        filter: &SearchFilter,
    ) -> Result<Vec<crate::history::HistoryHit>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        let window = filter.window;
        let qv = self.embedder.embed_one(query)?;
        let mut hits = self
            .with_history(|s| -> Result<Vec<crate::history::HistoryHit>> {
                let k = (limit * 8).clamp(16, 64)
                    * if window.is_open() {
                        1
                    } else {
                        self.store.config().policy.window_overfetch.max(1)
                    };
                let mut session_titles = std::collections::HashMap::new();
                for n in s.nodes_by_type_active(
                    &crate::types::NodeType::parse(crate::history::SESSION_TYPE)?,
                    usize::MAX,
                )? {
                    if let Some(sid) = &n.session_id {
                        session_titles.insert(sid.clone(), n.title);
                    }
                }
                let mk_hit = |n: &Node, score: f64| {
                    let p = |k: &str| n.props.as_ref().and_then(|m| m.get(k).cloned());
                    let session = n.session_id.clone().unwrap_or_default();
                    crate::history::HistoryHit {
                        message_id: n.id.clone(),
                        session_title: session_titles
                            .get(&session)
                            .map(|t| self.unseal_str(t))
                            .unwrap_or_default(),
                        session,
                        harness: p("harness").and_then(|v| v.as_str().map(str::to_string)),
                        role: p("role")
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_else(|| "assistant".into()),
                        turn: p("turn").and_then(|v| v.as_u64()),
                        timestamp: n.created_at,
                        snippet: crate::harvest::truncate_words(
                            &self.unseal_str(n.body.as_deref().unwrap_or(&n.title)),
                            240,
                        ),
                        score,
                        prior: Vec::new(),
                    }
                };
                // The keyword channel: history finally has BM25 —
                // the same blind index the curated store runs, identical
                // sealed or plaintext. Fused with search_hybrid's weights so
                // both layers rank by one rulebook; scores still NEVER
                // blend across layers.
                let pol = self.store.config().policy.clone();
                let kw_hits = s.search_fts(query, &[], k)?;
                let kw_max = kw_hits.iter().map(|h| h.score).fold(0.0f64, f64::max);
                let kw_norm: std::collections::HashMap<String, f64> = kw_hits
                    .iter()
                    .filter(|_| kw_max > 0.0)
                    .map(|h| (h.id.clone(), h.score / kw_max))
                    .collect();
                let sem_scaled = |sem: f64| {
                    ((sem - pol.semantic_floor) / (1.0 - pol.semantic_floor)).clamp(0.0, 1.0)
                };
                let mut out = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for (id, dist) in s.search_vec(&qv, k)? {
                    // Claim chunks share the node id — first (closest) wins.
                    if !seen.insert(id.clone()) {
                        continue;
                    }
                    let Some(n) = s.get_node(&id)? else { continue };
                    if n.node_type.as_str() != crate::history::MESSAGE_TYPE {
                        continue;
                    }
                    // A message's created_at IS when it was said — the one
                    // place in this system where the capture clock and the
                    // event clock are the same clock.
                    if !window.contains(n.created_at) {
                        continue;
                    }
                    let kw = kw_norm.get(&id).copied().unwrap_or(0.0);
                    let sem = (1.0 - dist).clamp(0.0, 1.0);
                    let score =
                        pol.keyword_weight * kw + (1.0 - pol.keyword_weight) * sem_scaled(sem);
                    out.push(mk_hit(&n, score));
                }
                // Keyword-only candidates: matched terms but slipped the
                // vector net — exactly the class the old vector-only path
                // could never see.
                for h in &kw_hits {
                    if !seen.insert(h.id.clone()) {
                        continue;
                    }
                    let Some(n) = s.get_node(&h.id)? else {
                        continue;
                    };
                    if n.node_type.as_str() != crate::history::MESSAGE_TYPE
                        || !window.contains(n.created_at)
                    {
                        continue;
                    }
                    let kw = kw_norm.get(&h.id).copied().unwrap_or(0.0);
                    out.push(mk_hit(&n, pol.keyword_weight * kw));
                }
                Ok(out)
            })
            .transpose()?
            .unwrap_or_default();
        if let Some(reranker) = &self.reranker
            && !hits.is_empty()
        {
            // Re-score over the FULL text (re-read per candidate — the same
            // shape the encrypted store needs later: decrypt N candidates in
            // memory at query time, nothing persistent).
            let docs: Vec<String> = hits
                .iter()
                .map(|h| {
                    self.with_history(|s| {
                        s.get_node(&h.message_id)
                            .ok()
                            .flatten()
                            .and_then(|n| n.body)
                            .map_or_else(|| h.snippet.clone(), |b| self.unseal_str(&b))
                    })
                    .unwrap_or_else(|| h.snippet.clone())
                })
                .collect();
            if let Ok(logits) = reranker.rank(query, &docs) {
                for (h, l) in hits.iter_mut().zip(logits) {
                    h.score = 1.0 / (1.0 + (-f64::from(l)).exp());
                }
            }
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        // The same session-diverse cut the curated path runs: dialogue is
        // where one session's restatements crowd hardest (an empty session
        // string means "unknown" and is never demoted).
        session_diverse_select(
            &mut hits,
            limit,
            self.store.config().policy.session_diversity_demote,
            |h| Some(h.session.as_str()).filter(|s| !s.is_empty()),
        );
        // Section gate, not per-hit trim (measured 2026-08-11): the per-hit
        // delivery floor halved false-fall-through noise but cost 0.03
        // end-to-end and 0.09 oblique dialogue recall — the 0.8.1 register
        // lesson (0.22 was calibrated on curated notes, not chat). Gating on
        // the TOP hit instead makes an all-noise section vanish while a
        // section with any real match keeps its full candidate list — low-
        // ranked gold survives.
        if self.reranker.is_some()
            && hits
                .first()
                .is_none_or(|top| top.score < self.store.config().policy.delivery_floor)
        {
            hits.clear();
        }
        // Ordering is the last word, after the gate — so a time-ordered read
        // is the same SET the relevance-ordered read would have delivered.
        order_history_hits(&mut hits, filter.order);
        if filter.order == crate::timespec::SearchOrder::Recent {
            hits = self.collapse_restatements(hits)?;
        }
        Ok(hits)
    }

    /// Fold restatements under their newest form, for `order:
    /// "recent"` only.
    ///
    /// A transcript has no `replaces` edges — nobody curates dialogue — so the
    /// supersession chain the curated graph gets from judgment has to be
    /// inferred here from the text itself. Hits arrive newest-first, so the
    /// first statement of a thing IS its latest statement; every later
    /// near-duplicate becomes one of its `prior` generations.
    ///
    /// Three things keep this honest. It runs ONLY under the recency ordering,
    /// so the default path is untouched and owes no receipt. It never removes
    /// a hit — folding is nesting, so recall is unchanged BY CONSTRUCTION and
    /// a mistuned threshold costs shape, not answers. And the threshold is a
    /// per-graph knob (`history.recency_collapse`), because this project has
    /// already learned once that absolute score thresholds don't transfer
    /// between registers.
    fn collapse_restatements(
        &self,
        hits: Vec<crate::history::HistoryHit>,
    ) -> Result<Vec<crate::history::HistoryHit>> {
        let threshold = self.store.config().history.recency_collapse;
        if hits.len() < 2 || !(0.0..1.0).contains(&threshold) {
            return Ok(hits);
        }
        let texts: Vec<String> = hits.iter().map(|h| h.snippet.clone()).collect();
        let vecs = self.embedder.embed(&texts)?;
        if vecs.len() != hits.len() {
            return Ok(hits);
        }
        let mut heads: Vec<(usize, crate::history::HistoryHit)> = Vec::new();
        for (i, hit) in hits.into_iter().enumerate() {
            let head = heads
                .iter_mut()
                .find(|(j, _)| cosine(&vecs[*j], &vecs[i]) >= threshold);
            match head {
                Some((_, head)) => head.prior.push(hit),
                None => heads.push((i, hit)),
            }
        }
        Ok(heads.into_iter().map(|(_, h)| h).collect())
    }

    /// Every message of one session, decrypted in memory and in
    /// conversation order — the pane's history view reads whole sessions.
    pub fn history_messages(
        &self,
        session: &str,
    ) -> Result<Vec<crate::history::HistoryMessageView>> {
        let msgs = self
            // Indexed: only this conversation's nodes are read and
            // unsealed, not the whole recording.
            .with_history(|s| -> Result<Vec<crate::types::Node>> {
                Ok(s.nodes_in_session(session)?
                    .into_iter()
                    .filter(|n| n.node_type.as_str() == crate::history::MESSAGE_TYPE)
                    .collect())
            })
            .transpose()?
            .unwrap_or_default();
        let mut views: Vec<crate::history::HistoryMessageView> = msgs
            .into_iter()
            .map(|n| {
                let p = |k: &str| n.props.as_ref().and_then(|m| m.get(k).cloned());
                crate::history::HistoryMessageView {
                    role: p("role")
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_else(|| "assistant".into()),
                    turn: p("turn").and_then(|v| v.as_u64()),
                    timestamp: n.created_at,
                    text: self.unseal_str(&n.body.unwrap_or_default()),
                    message_id: n.id,
                }
            })
            .collect();
        views.sort_by_key(|v| (v.turn.unwrap_or(u64::MAX), v.timestamp));
        Ok(views)
    }

    /// The exchange around one turn of a session, decrypted in memory —
    /// `expand_history(session, turn, window)`. The model decides how much
    /// raw dialogue to spend context on; nothing is auto-injected.
    pub fn expand_history(
        &self,
        session: &str,
        turn: u64,
        window: u64,
    ) -> Result<Vec<crate::history::HistoryMessageView>> {
        let views = self.history_messages(session)?;
        if views.is_empty() {
            return Ok(views);
        }
        let center = views
            .iter()
            .position(|v| v.turn == Some(turn))
            .unwrap_or_else(|| {
                // Nearest by turn when the exact one was filtered/absent.
                views
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.turn.unwrap_or(u64::MAX).abs_diff(turn))
                    .map_or(0, |(i, _)| i)
            });
        let window = (window as usize).min(views.len());
        let lo = center.saturating_sub(window);
        let hi = (center + window + 1).min(views.len());
        Ok(views[lo..hi].to_vec())
    }

    /// Every recorded session, newest lane first — the history browser's
    /// spine.
    /// Sessions overlapping a time window, newest lane first — the
    /// "what was I doing last Tuesday" entry point, and the surface that makes
    /// a temporal question answerable WITHOUT a search hit to start from.
    ///
    /// A session counts as inside the window when any part of it is: a session
    /// that began before the window and ran into it is exactly the one a
    /// person asking about that window means.
    pub fn list_history_sessions_in(
        &self,
        window: crate::timespec::TimeWindow,
    ) -> Result<Vec<crate::history::HistorySessionView>> {
        let mut out = self.list_history_sessions()?;
        if !window.is_open() {
            out.retain(|s| {
                let ended = s.ended.unwrap_or(s.started);
                window.after.is_none_or(|a| ended >= a)
                    && window.before.is_none_or(|b| s.started < b)
            });
        }
        Ok(out)
    }

    pub fn list_history_sessions(&self) -> Result<Vec<crate::history::HistorySessionView>> {
        let mut out: Vec<crate::history::HistorySessionView> = self
            // Indexed by type: the Session nodes alone, not every
            // recorded message.
            .with_history(|s| -> Result<Vec<crate::types::Node>> {
                Ok(s.nodes_of_type(crate::history::SESSION_TYPE)?
                    .into_iter()
                    .filter(|n| n.valid_until.is_none())
                    .collect())
            })
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .map(|n| {
                let p = |k: &str| n.props.as_ref().and_then(|m| m.get(k).cloned());
                crate::history::HistorySessionView {
                    session: n.session_id.clone().unwrap_or_default(),
                    title: self.unseal_str(&n.title),
                    harness: p("harness").and_then(|v| v.as_str().map(str::to_string)),
                    started: n.created_at,
                    ended: p("ended").and_then(|v| v.as_i64()),
                    messages: p("messages").and_then(|v| v.as_u64()).unwrap_or(0),
                    version: n.version,
                    node_id: n.id,
                }
            })
            .collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.started));
        Ok(out)
    }

    /// Per-session hard delete (pane-only, like every hard delete): the
    /// Session node, its messages, and their edges leave the history store —
    /// and the session's transcript path joins `history.exclude_paths`, or
    /// the harvester would quietly resurrect it from the file that still
    /// sits on disk. The exclusion is visible (and reversible) in settings.
    pub fn delete_history_session(&self, session: &str) -> Result<usize> {
        let (removed, transcript) = self
            .with_history(|s| -> Result<(usize, Option<String>)> {
                let nodes: Vec<Node> = s.nodes_in_session(session)?;
                let transcript = nodes
                    .iter()
                    .find(|n| n.node_type.as_str() == crate::history::SESSION_TYPE)
                    .and_then(|n| n.props.as_ref())
                    .and_then(|p| p.get("path"))
                    .and_then(|v| v.as_str().map(str::to_string));
                for n in &nodes {
                    s.delete_node(&n.id)?;
                }
                Ok((nodes.len(), transcript))
            })
            .transpose()?
            .unwrap_or((0, None));
        if let Some(path) = transcript {
            let mut cfg = (*self.store.config()).clone();
            if !cfg.history.exclude_paths.contains(&path) {
                cfg.history.exclude_paths.push(path);
                self.set_graph_config(&cfg)?;
            }
        }
        if removed > 0 {
            self.audit(
                "history_session_deleted",
                "graph",
                session,
                Some(format!("{removed} recorded node(s) removed")),
                None,
                None,
                None,
            )?;
        }
        Ok(removed)
    }

    /// Counts for the pane's History settings (None = layer closed).
    pub fn history_stats(&self) -> Option<crate::StoreStats> {
        self.with_history(|s| s.stats().ok()).flatten()
    }

    /// Wholesale history delete — the user-only escape hatch the sibling
    /// store exists for. Drops the handle, removes the file, and (when the
    /// layer is enabled) reopens fresh with a re-seeded ontology. The caller
    /// (HTTP layer) also bumps the hub's history epoch so the running
    /// harvester forgets its cursors and caches.
    pub fn reset_history(&self) -> Result<()> {
        let Some(path) = self.history_path.clone() else {
            return Ok(());
        };
        {
            let mut slot = self.history.locked();
            *slot = None; // close before deleting — redb is single-handle
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| crate::Error::Io(format!("removing {}: {e}", path.display())))?;
            }
        }
        self.sync_history_store();
        self.audit(
            "history_reset",
            "graph",
            "",
            Some("history layer wiped by the user".into()),
            None,
            None,
            None,
        )?;
        Ok(())
    }

    /// Replace a history node in place (the harvester's cursor bumps and
    /// title upgrades live in `Session.props`). A legacy-sealed field
    /// arriving from an old read path is opened first; storage-level
    /// sealing is the driver's job. Re-embeds only when asked, always from
    /// plaintext. Returns whether the layer was open to take the write.
    pub fn upsert_history_node(&self, node: &Node, re_embed: bool) -> Result<bool> {
        let mut stored = node.clone();
        stored.title = self.unseal_str(&stored.title);
        stored.body = stored.body.as_deref().map(|b| self.unseal_str(b));
        let wrote = self
            .with_history(|s| {
                s.upsert_node(&stored)?;
                Ok::<_, crate::Error>(())
            })
            .transpose()?;
        if wrote.is_some() && re_embed {
            let mut texts = vec![embed_text(
                &stored.title,
                stored.body.as_deref(),
                &stored.tags,
                &stored.code_refs,
            )];
            texts.extend(claim_texts(&stored.title, stored.body.as_deref()));
            let vectors = self.embedder.embed(&texts)?;
            self.with_history(|s| s.upsert_embeddings(&stored.id, &vectors))
                .transpose()?;
        }
        Ok(wrote.is_some())
    }

    /// Reconcile the open handle with `config().history.enabled` — runs on
    /// path install and after every config write, so the pane's toggle takes
    /// effect without a daemon restart. Disabling drops the handle; the file
    /// stays on disk (delete is a user gesture). A freshly-opened store is
    /// also reconciled with the daemon's desired encryption, which
    /// seals pre-0.9 plaintext/mixed history stores on first open.
    pub(super) fn sync_history_store(&self) {
        let mut slot = self.history.locked();
        let enabled = self.store.config().history.enabled;
        if enabled && slot.is_none() {
            if let Some(path) = &self.history_path {
                match crate::history::open_history_store(path) {
                    Ok(s) => {
                        if let Err(e) =
                            reconcile_encryption(&*s, self.desired_history_enc, &mut |_, _| {})
                        {
                            eprintln!("engram: history encryption reconcile failed: {e}");
                        }
                        *slot = Some(s);
                    }
                    Err(e) => {
                        eprintln!(
                            "engram: couldn't open history store {}: {e}",
                            path.display()
                        );
                    }
                }
            }
        } else if !enabled && slot.is_some() {
            *slot = None;
        }
    }
}

/// Re-order history hits — the episodic twin of [`order_hits`]. Ties break by
/// message id (time-sortable), so messages sharing a second keep the order
/// they were said in.
fn order_history_hits(
    hits: &mut [crate::history::HistoryHit],
    order: crate::timespec::SearchOrder,
) {
    use crate::timespec::SearchOrder;
    match order {
        SearchOrder::Relevance => {}
        SearchOrder::Chronological => {
            hits.sort_by(|a, b| (a.timestamp, &a.message_id).cmp(&(b.timestamp, &b.message_id)));
        }
        SearchOrder::Recent => {
            hits.sort_by(|a, b| (b.timestamp, &b.message_id).cmp(&(a.timestamp, &a.message_id)));
        }
    }
}
