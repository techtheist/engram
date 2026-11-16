//! Read tools: search, nodes, history, the brief, the ontology, the journal.

use crate::*;

#[tool_router(router = read_router, vis = "pub(crate)")]
impl Engram {
    #[tool(
        description = "Hybrid semantic + keyword search over the memory graph. \
        Respect the reply's `confidence` verdict: `strong` = the top hit cleared \
        this graph's calibrated line; `weak` = likely not in memory, the hits \
        are nearest candidates — verify before relying; `none` = the graph is \
        silent — say so instead of inventing a memory. Every hit carries a \
        `standing` verdict — canon (the user endorsed it), confirmed, \
        unverified (a lead, not a fact), stale (verify first), contested (an \
        open conflict: tell the user), superseded (read the replacement), \
        tombstone (deliberately removed) — plus trust (0..1) and 1-hop \
        neighbors (conflicts/replaces first). \
        Temporal questions: scope with `after`/`before`/`during_version` \
        (the daemon resolves relative phrases — never compute dates yourself) \
        and `order` (chronological = how it developed, recent = current value); \
        `date_field` aims the window at a date-kind custom field (event time) \
        instead of created_at (capture time)."
    )]
    pub(crate) async fn search(
        &self,
        Parameters(a): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let types = node_types(&a.types)?;
        let limit = a.limit.unwrap_or(8);
        let detail = Detail::parse(a.detail.as_deref())?;
        if a.project.as_deref() == Some(registry::ALL_PROJECTS) {
            let (mut hits, skipped) = self
                .hub
                .search_all(&a.query, &types, limit)
                .map_err(map_err)?;
            hits.iter_mut().for_each(debracket);
            let hits = self.shape_hits(&detail, hits, None);
            return self.reply(&json!({ "hits": hits, "skipped": skipped }));
        }
        let engine = self.engine_for(&a.project)?;
        let scope = a.scope.as_deref().unwrap_or("auto");
        if !matches!(scope, "auto" | "memory" | "history") {
            return Err(ErrorData::invalid_params(
                format!("scope {scope:?} must be auto, memory or history"),
                None,
            ));
        }
        // One filter grammar for both layers, resolved once against the version
        // journal. The event clock reads custom fields, which history lacks.
        if scope == "history"
            && a.date_field
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
        {
            return Err(ErrorData::invalid_params(
                "date_field applies to curated memory — history has no custom fields".to_string(),
                None,
            ));
        }
        let filter = {
            let guard = engine.locked();
            guard
                .time_filter_clocked(
                    a.after.as_deref(),
                    a.before.as_deref(),
                    a.during_version.as_deref(),
                    a.order.as_deref(),
                    a.date_field.as_deref(),
                )
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?
        };
        if scope == "history" {
            let hits = {
                let guard = self.mcp(&engine);
                guard
                    .search_history_filtered(&a.query, limit, &filter)
                    .map_err(map_err)?
            };
            return self.reply(&json!({
                "history": { "hits": hits, "note": HISTORY_SECTION_NOTE },
            }));
        }
        let (mut hits, confidence, tombstoned) = {
            let guard = self.mcp(&engine);
            let hits = guard
                .search_filtered(&a.query, &types, limit, &filter)
                .map_err(map_err)?;
            let confidence = guard.search_confidence(&hits);
            // Count tombstone hits so the reply says a hit is a removal record, not canon.
            let tombstoned = hits.iter().filter(|h| h.tombstone).count();
            (hits, confidence, tombstoned)
        };
        hits.iter_mut().for_each(debracket);
        let hit_ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
        // Found bridges: the top few hits name one note a bridge
        // note ties them to while no edge does — kept small, and left off
        // the compact scan.
        let bridge_ids: Vec<String> = hits
            .iter()
            .filter(|h| !h.tombstone)
            .take(SEARCH_BRIDGE_HITS)
            .map(|h| h.id.clone())
            .collect();
        let mut hits = self.shape_hits(&detail, hits, Some(&engine));
        if !matches!(detail, Detail::Compact)
            && let Some(arr) = hits.as_array_mut()
        {
            let found = engine
                .locked()
                .found_bridges_many(&bridge_ids, 1)
                .unwrap_or_default();
            for (id, f) in bridge_ids.iter().zip(found) {
                if f.is_empty() {
                    continue;
                }
                if let Some(shaped) = arr.iter_mut().find(|h| h["id"] == json!(id)) {
                    shaped["bridges"] = json!(f);
                }
            }
        }
        // Provenance line: a curated hit born in a recorded exchange
        // says so — the birth dialogue is one expand_history away.
        if let Some(arr) = hits.as_array_mut() {
            let guard = engine.locked();
            for (shaped, id) in arr.iter_mut().zip(&hit_ids) {
                if let Some(born) = guard.born_in_of(id) {
                    shaped["born_in"] = json!(born);
                }
            }
        }
        let mut body = json!({ "hits": hits });
        // Verdict-routed fall-through (decision 00bgftfdusll): history is
        // queried only when the calibrated verdict says the curated graph
        // likely doesn't hold the answer — a separate labeled section,
        // never interleaved, never score-blended.
        if scope == "auto" && matches!(confidence, Some("weak" | "none")) {
            let guard = engine.locked();
            if guard.config().history.search_fallthrough && guard.history_open() {
                drop(guard);
                let history = {
                    // History has no custom fields: the fall-through reads
                    // the same window on the capture clock.
                    let mut hf = filter.clone();
                    hf.clock = Default::default();
                    let guard = self.mcp(&engine);
                    guard
                        .search_history_filtered(&a.query, limit, &hf)
                        .map_err(map_err)?
                };
                if !history.is_empty() {
                    body["history"] = json!({
                        "hits": history,
                        "note": HISTORY_SECTION_NOTE,
                    });
                }
            }
        }
        if let Some(v) = confidence {
            body["confidence"] = json!(v);
            let note = match v {
                "none" => Some(
                    "The graph is silent on this — nothing in memory answers it. \
                     Do not infer a remembered answer; if the fact surfaces in \
                     this session, capture it.",
                ),
                "weak" => Some(
                    "This likely isn't in memory: no hit cleared this graph's \
                     calibrated confidence line, so what follows are the nearest \
                     candidates, not a found answer — delivered anyway, never \
                     cut. Verify against the code or the user before relying \
                     on any of them.",
                ),
                _ => None,
            };
            if let Some(n) = note {
                body["note"] = json!(n);
            }
        }
        if tombstoned > 0 {
            body["tombstone_note"] = json!(format!(
                "{tombstoned} hit{} carr{} the tombstone role: a record that \
                 knowledge was deliberately removed, not live canon. Do not \
                 re-add or re-derive what it names unless the user says so — \
                 its body says why it was removed.",
                if tombstoned == 1 { "" } else { "s" },
                if tombstoned == 1 { "ies" } else { "y" },
            ));
        }
        self.reply(&body)
    }

    #[tool(description = "Read the recorded dialogue around one turn: takes the \
        `session` and `turn` a history hit (or a node's `born_in`) carried; \
        `window` = context each side (default 4). Raw dialogue, not curated \
        memory. The reply's `notes` lists curated nodes born in that session.")]
    pub(crate) async fn expand_history(
        &self,
        Parameters(a): Parameters<ExpandHistoryArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let window = a.window.unwrap_or(4).min(25);
        let (messages, notes) = {
            let guard = self.mcp(&engine);
            let messages = guard
                .expand_history(&a.session, a.turn, window)
                .map_err(map_err)?;
            let notes = guard.session_notes(&a.session).map_err(map_err)?;
            (messages, notes)
        };
        if messages.is_empty() {
            return Err(ErrorData::invalid_params(
                format!("no recorded messages for session {:?}", a.session),
                None,
            ));
        }
        let mut body = json!({ "session": a.session, "messages": messages });
        if !notes.is_empty() {
            body["notes"] = json!(notes);
        }
        self.reply(&body)
    }

    #[tool(
        description = "List recorded sessions, newest first, with the `session` \
        handle expand_history takes. Scope with `after`/`before` to answer \
        \"what was I working on then\" without a search hit. Raw history, not \
        curated memory; empty when recording is off."
    )]
    pub(crate) async fn list_sessions(
        &self,
        Parameters(a): Parameters<ListSessionsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let window = engram_core::timespec::window(
            a.after.as_deref(),
            a.before.as_deref(),
            engram_core::now(),
        )
        .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let mut sessions = {
            let guard = self.mcp(&engine);
            guard.list_history_sessions_in(window).map_err(map_err)?
        };
        if let Some(h) = &a.harness {
            sessions.retain(|s| s.harness.as_deref() == Some(h.as_str()));
        }
        let limit = a.limit.unwrap_or(20).min(100);
        let total = sessions.len();
        sessions.truncate(limit);
        let mut body = json!({ "sessions": sessions, "total": total });
        if total == 0 {
            body["note"] = json!(if window.is_open() {
                "Nothing recorded — session recording is off (the user's switch, in the \
                 pane) or no transcripts have been ingested yet."
            } else {
                "No sessions in that window. Recording may be off, or nothing was \
                 recorded then — widen the window before concluding the work never \
                 happened."
            });
        }
        self.reply(&body)
    }

    #[tool(
        description = "Fetch one node by id with its edges, computed trust/stale, \
        and `born_in` provenance when recorded (expand_history reads the birth \
        dialogue). `parents`/`children` (depth 0-3) walk the reasoning hierarchy: \
        parents = what it points at (its reasons), children = what points at it. \
        `bridges` (when present) = notes a bridge note ties to this one while no \
        edge does (`via` = the bridge): read-only hints, not judgements."
    )]
    pub(crate) async fn get_node(
        &self,
        Parameters(a): Parameters<GetNodeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let engine = self.mcp(&engine);
        let Some(node) = engine.get_node(&a.id).map_err(map_err)? else {
            return Err(ErrorData::invalid_params(
                format!("node not found: {}", a.id),
                None,
            ));
        };
        let out = engine.edges_out(&a.id).map_err(map_err)?;
        let incoming = engine.edges_in(&a.id).map_err(map_err)?;
        let mut payload = json!({ "node": node, "edges_out": out, "edges_in": incoming });
        // Provenance: same line search hits carry — where this note
        // was born, when the history layer recorded the exchange.
        if let Some(born) = engine.born_in_of(&a.id) {
            payload["born_in"] = json!(born);
        }
        // Found bridges: notes a bridge note ties to this one while
        // no edge does — a connection the text states, not a judgement.
        let bridges = engine.found_bridges(&a.id, NODE_BRIDGES).map_err(map_err)?;
        if !bridges.is_empty() {
            payload["bridges"] = json!(bridges);
        }
        let up = a.parents.unwrap_or(0).min(HIERARCHY_MAX_DEPTH);
        let down = a.children.unwrap_or(0).min(HIERARCHY_MAX_DEPTH);
        if up > 0 {
            let mut seen = std::collections::HashSet::from([a.id.clone()]);
            payload["parents"] = json!(hierarchy(&engine, &a.id, up, true, &mut seen));
        }
        if down > 0 {
            let mut seen = std::collections::HashSet::from([a.id.clone()]);
            payload["children"] = json!(hierarchy(&engine, &a.id, down, false, &mut seen));
        }
        self.reply(&payload)
    }

    #[tool(
        description = "Bounded breadth-first subgraph around a node, optionally \
        filtered to specific edge types."
    )]
    pub(crate) async fn traverse(
        &self,
        Parameters(a): Parameters<TraverseArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let edge_types = edge_types(&a.edge_types)?;
        let engine = self.engine_for(&a.project)?;
        let (nodes, edges) = self
            .mcp(&engine)
            .traverse(&a.from, &edge_types, a.depth.unwrap_or(2))
            .map_err(map_err)?;
        self.reply(&json!({ "nodes": nodes, "edges": edges }))
    }

    #[tool(
        description = "Token-budgeted markdown digest of the graph: conflicts, \
        open work, canon, recent changes. Call once when starting work. \
        Passing `project` REBINDS this session to that registered project \
        first and returns ITS brief — the one call that both selects the \
        workspace's project and loads its memory when the client answered no \
        MCP roots. Unknown selectors are refused with the known roster."
    )]
    pub(crate) async fn brief(
        &self,
        Parameters(a): Parameters<BriefArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // A scoped call rebinds (issue #4). Resolve first so a refused selector leaves
        // the binding alone; only registered projects resolve, so no graph is born here.
        if let Some(sel) = &a.project {
            let engine = self.hub.get(sel).map_err(map_err)?;
            let id = self.hub.resolve_id(sel).map_err(map_err)?;
            let trace = SessionTrace::start(
                &engine,
                &self.session_id,
                Some(format!("project {id} via brief")),
            );
            let leaving = {
                let mut binding = self.binding.writing();
                let leaving = (
                    binding
                        .bound
                        .as_deref()
                        .map_or_else(|| self.hub.current().id.clone(), str::to_string),
                    binding.bound_by,
                );
                binding.engine = engine.clone();
                binding.bound = Some(id.as_str().into());
                // The agent chose this binding itself: the fallback write
                // guard and the first-write note stand down.
                binding.bound_by = BoundBy::Brief;
                // Dropping the old trace journals mcp_session_ended in the
                // graph this session is leaving.
                binding.trace = trace;
                leaving
            };
            self.hub.session_bind(&self.session_id, Some(&id));
            self.hub.session_meta(
                &self.session_id,
                self.session_client(),
                BoundBy::Brief.as_str(),
            );
            let max_chars = engine.locked().brief_chars(a.max_chars);
            let brief = self.hub.brief_for(Some(&id), max_chars).map_err(map_err)?;
            let text = format!(
                "{}\n\n{brief}",
                self.rebind_line(&id, &leaving.0, leaving.1)
            );
            return Ok(CallToolResult::success(vec![ContentBlock::text(text)]));
        }
        // Unscoped = THIS SESSION's project plus the home-graph section. The
        // session's binding is the whole point: the hub's own current project
        // is the core's launch graph (home), which is nobody's answer.
        let max_chars = self.session_engine().locked().brief_chars(a.max_chars);
        let mut text = self
            .hub
            .brief_for(self.session_bound().as_deref(), max_chars)
            .map_err(map_err)?;
        if let Some(hint) = self.fallback_hint() {
            text = format!("{hint}\n\n{text}");
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(description = "This graph's ontology: every node type and edge verb, \
        with roles and examples (ontologies are per-graph). Call before your \
        first write into any graph you haven't described yet — an empty or \
        unfamiliar graph especially — and whenever names surprise you.")]
    pub(crate) async fn describe_ontology(
        &self,
        Parameters(a): Parameters<DescribeOntologyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let text = engine.locked().config().describe_ontology();
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(
        description = "Check a claim against the graph with the local NLI model: \
        {supports, contradicts, retracted, silent}. Use before acting on an \
        assumption; contradicts-hits are conflicts to surface, retracted = a \
        Tombstone says this was deliberately removed (don't act on it), \
        all-silent on a real topic is a gap worth capturing. Verdicts are \
        small-model hints — you judge."
    )]
    pub(crate) async fn check_claim(
        &self,
        Parameters(a): Parameters<CheckClaimArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = a.limit.unwrap_or(8);
        if a.project.as_deref() == Some(registry::ALL_PROJECTS) {
            let (report, skipped) = self.hub.check_claim_all(&a.claim, limit).map_err(map_err)?;
            let mut out = json!(report);
            out["skipped"] = json!(skipped);
            return self.reply(&out);
        }
        let engine = self.engine_for(&a.project)?;
        let report = self
            .mcp(&engine)
            .check_claim(&a.claim, limit)
            .map_err(map_err)?;
        self.reply(&report)
    }

    #[tool(
        description = "A node's replaces chain, oldest first, each generation \
        with the note that retired it — how a decision evolved."
    )]
    pub(crate) async fn timeline(
        &self,
        Parameters(a): Parameters<IdArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let chain = self.mcp(&engine).timeline(&a.id).map_err(map_err)?;
        self.reply(&json!({ "timeline": chain }))
    }

    #[tool(description = "Nodes whose code_refs no longer resolve — code moved, \
        memory didn't. Fix refs via update_node; supersede or conflict the \
        knowledge if it's no longer true.")]
    pub(crate) async fn list_drift(
        &self,
        Parameters(a): Parameters<ProjectArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let engine = self.mcp(&engine);
        // A scoped project's refs resolve against *its* repo root; the cwd is
        // only the launch project's fallback.
        let root = match engine.repo_root() {
            Some(r) => r.to_path_buf(),
            None => std::env::current_dir()
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
        };
        let drifted = engine.scan_code_refs(&root).map_err(map_err)?;
        self.reply(&json!({ "drifted": drifted }))
    }

    #[tool(description = "Audit journal page, newest first: every mutation with \
        before/after snapshots and writer context. Filter with entity_id; page \
        with before = the last row's seq.")]
    pub(crate) async fn audit(
        &self,
        Parameters(a): Parameters<AuditArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let page = self
            .mcp(&engine)
            .audit_log(
                a.before,
                a.entity_id.as_deref(),
                a.limit.unwrap_or(20).min(200),
            )
            .map_err(map_err)?;
        self.reply(&page)
    }

    #[tool(description = "Every graph this hub reaches: current project, the \
        user-level home graph, and the machine registry. The names are the \
        `project` argument other tools accept: omit = current, name = that \
        project, 'home' = shared user graph, 'all' = fan-out reads only.")]
    pub(crate) async fn list_projects(&self) -> Result<CallToolResult, ErrorData> {
        // `current` is THIS SESSION's project, not the core's launch graph —
        // a bound session told it is in `home` reads as the wrong graph
        // being open and sends the assistant looking for its memory elsewhere.
        self.reply(&json!({ "projects": self.hub.projects_for(self.session_bound().as_deref()) }))
    }
}
