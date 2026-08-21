//! stdio MCP server (`rmcp`) exposing the Engram graph to Claude. Thin wrapper
//! over `engram_core::Engine` implementing the Appendix A tool contracts. Note:
//! `delete_node` is deliberately absent — hard delete is user-only (PLAN §6B),
//! so Claude has no tool for it.

use std::sync::{Arc, Mutex};

use engram_core::{
    Durability, EdgePatch, EdgeStatus, EdgeType, Engine, Error, Hub, NewEdge, NewNode, Node,
    NodePatch, NodeStatus, NodeType, Source, SuspectVerdict, WriteOutcome, registry,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourceTemplatesResult, ListResourcesResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult, Resource,
    ResourceContents, ResourceTemplate, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

const INSTRUCTIONS: &str = "\
Engram is the project's durable reasoning/decision memory as an editable graph. \
Call `brief` at the start of a session for a compact digest of the canon \
(conflicts, open work, principles, decisions, cautions). Use `search` before \
non-trivial work — hits carry their 1-hop neighbors, conflicts and supersessions \
first. Capture every decision as it happens — a feature request usually hides \
one (library picked, shape chosen, tradeoff accepted) and it belongs in the \
graph even though nobody said \"remember this\". Every write's response is a \
verdict, not a receipt: `add_note` returns {matched, created:false} on a \
near-duplicate (merge via `update_node`; when SEVERAL notes state the same \
knowledge, consolidate them with `merge_nodes` — it rehomes their edges \
instead of stranding them), `warnings` when the text lands near \
contradicted or superseded knowledge, and `suspects` when it queued unjudged \
look-alike pairs — judge those immediately with `resolve_suspect` and tell \
the user when one is a genuine contradiction; that alert is the one exception \
to silent capture. \
Link nodes with sentence-shaped edges (e.g. a Decision `because` a Principle); \
repair a wrong link with `unlink` / `update_edge`. When the brief lists \
suspected conflicts, judge them early via `resolve_suspect` (conflict | \
replaces | dismiss) — the scan only finds candidates; you are the judge. \
Nodes carry computed \
`trust` (0..1) and `stale` (trust < 0.3 — verify before relying). Trust reads \
only deliberate acts: updates (confirmed_at) and approvals; retrieval never \
refreshes it — being findable proves nothing. Stable-durability knowledge \
holds its trust flat until a judged conflict demotes it (withdrawing the \
conflict withdraws the demotion; drift is review-only and never demotes); \
episodic/volatile knowledge decays with time. If a stale node is still true, \
say so with `update_node` — that is what restores trust. Pinned nodes \
(constant trust, set by the user in the pane) are marked PINNED in the brief; \
pinning and unpinning are user-only gestures, and a `replaces` verdict that \
would archive a pinned node is refused — surface it to the user instead. \
Nodes can carry free-form `tags` — how the user slices the graph (phases, \
concerns). Reuse the recent tags the brief lists before inventing new ones; \
an unknown tag is simply created. \
`check_claim` verifies a statement against the canon via the local NLI \
model ({supports, contradicts, silent}) — use it before acting on an \
assumption; its verdicts are hints, never judgments. \
For history questions: `timeline` walks a node's replaces chain (\"how did \
this decision evolve\"), `audit` pages the mutation journal (\"what changed, \
who wrote this\"). `list_drift` finds nodes whose code_refs no longer exist \
in the project — repair the refs via `update_node` and re-check the claim. \
For whole-graph work, use the bulk tools: `list_nodes` pages complete nodes \
(full bodies — the lossless read behind \"export every Decision to a \
decisions.md\"), `update_nodes` applies many patches in one call (curation \
sweeps), `add_notes` batch-creates with the same dupe checks as add_note. \
Most tools take an optional `project`: omit it for this project; a name, an \
id, or a project's directory (see `list_projects`) reads or writes THAT \
project's graph — capturing an insight about a sibling project into its own \
graph is deliberate and encouraged; `home` is the user-level graph for knowledge that transcends \
projects (global principles, preferences — write there on \"remember this \
globally\"); `search`/`check_claim` accept `project: \"all\"` to read across \
every graph (foreign hits carry provenance and a locality prior). Writes to \
`all` are refused — one insight lives in one graph, not N copies. \
Never store secrets or volatile implementation detail.";

/// Upper bound on items per batch tool call — big enough for any real
/// curation sweep, small enough to keep one call's audit burst readable.
const BATCH_CAP: usize = 100;

/// Attached to write responses that queued suspects: the write landed, but
/// the graph now holds an unjudged look-alike pair — the writer must close
/// that loop in the same turn, not leave it for the next session's brief.
const SUSPECT_ACTION: &str = "This note closely resembles existing unlinked knowledge (see `suspects`). \
Judge each pair NOW with resolve_suspect: `conflict` if they contradict (then tell the user — a live \
contradiction with standing canon is the one thing silent capture must surface), `replaces` if this \
write supersedes the older claim, `dismiss` if they are fine together.";

/// Journal bracket for one MCP session (activity audit): `mcp_session_started`
/// on construction, `mcp_session_ended` when the LAST clone of the session's
/// server drops (the server is `Clone`, so the bracket rides an `Arc`).
/// Best-effort by design — a session must never fail over its own trace.
struct SessionTrace {
    engine: Arc<Mutex<Engine>>,
    session_id: Arc<str>,
}

const VALIDATION_MIN_INTERVAL_SECS: i64 = 60;

/// The label every history section carries — raw dialogue is a different
/// register from curated memory and must never read as equivalent.
const HISTORY_SECTION_NOTE: &str = "From session history — raw recorded dialogue, not curated \
     memory. Snippets only; call expand_history(session, turn) to read the surrounding \
     exchange before relying on one.";

impl SessionTrace {
    fn start(
        engine: &Arc<Mutex<Engine>>,
        session_id: &Arc<str>,
        note: Option<String>,
    ) -> Arc<Self> {
        let trace = Arc::new(Self {
            engine: engine.clone(),
            session_id: session_id.clone(),
        });
        trace.emit("mcp_session_started", note);
        trace.validate();
        trace
    }

    fn emit(&self, action: &str, note: Option<String>) {
        if let Ok(mut engine) = self.engine.lock() {
            engine.set_audit_origin(engram_core::AuditOrigin::mcp(self.session_id.to_string()));
            let _ = engine.audit_activity(action, note);
        }
    }

    /// Run the full graph validation (decay + supersession + conflict scan +
    /// drift) in the
    /// background so connects stay instant — the graph is prepared for the
    /// session, and cleaned up after it, without waiting for the six-hourly
    /// sweep. Rate-limited PER GRAPH (the engine keeps the clock — a connect
    /// to one project must not suppress another's validation); skipped on
    /// fake embeddings (fake vectors over a real graph would queue noise
    /// suspects).
    fn validate(&self) {
        let engine = self.engine.clone();
        let session = self.session_id.clone();
        std::thread::spawn(move || {
            if let Ok(mut engine) = engine.lock() {
                if engine.embeddings_are_fake()
                    || !engine.validation_due(VALIDATION_MIN_INTERVAL_SECS)
                {
                    return;
                }
                engine.set_audit_origin(engram_core::AuditOrigin::mcp(session.to_string()));
                let _ = engine.validate_graph();
            }
        });
    }
}

impl Drop for SessionTrace {
    fn drop(&mut self) {
        self.emit("mcp_session_ended", None);
        self.validate();
    }
}

#[derive(Clone)]
pub struct Engram {
    /// Session lifecycle journal rows; ends when the last clone drops.
    _trace: Arc<SessionTrace>,
    /// The multi-project hub (PLAN §7C). Single-project constructions get a
    /// factory-less hub, so cross-project selectors fail with a clear message.
    hub: Arc<Hub>,
    /// This session's project engine — `hub.current()` for a launch-bound
    /// session, the bound project's for one minted by [`Engram::for_project`]
    /// — cached for the unscoped fast path.
    engine: Arc<Mutex<Engine>>,
    /// The selector this session is bound to, when it is bound to one project
    /// of a multi-project hub (`/projects/{id}/mcp`). `None` = the launch
    /// project. Every hub-level call that reports or renders "the current
    /// project" must pass this — the engine alone can't tell the hub which
    /// project it is looking at.
    bound: Option<Arc<str>>,
    /// Fallback session id when the client omits one: minted once per server
    /// process, which over stdio is one Claude session. Superseded by the
    /// transport session id after the streamable-HTTP migration (PLAN §0).
    session_id: Arc<str>,
    /// The mid-session conflict push (v0.6.3): judged contradictions from any
    /// session or project land here and ride out on this session's next tool
    /// response.
    conflicts: Arc<Mutex<engram_core::ConflictFeed>>,
}

#[tool_router]
impl Engram {
    pub fn new(engine: Engine) -> Self {
        Self::with_hub(Arc::new(Hub::single(engine)))
    }

    /// Build over an engine shared with the HTTP server (same DB + listener).
    pub fn with_shared(engine: Arc<Mutex<Engine>>) -> Self {
        Self::with_hub(Arc::new(Hub::single_shared(engine)))
    }

    /// The full multi-project form: the same hub the HTTP server holds.
    pub fn with_hub(hub: Arc<Hub>) -> Self {
        let engine = hub.current_engine();
        let session_id: Arc<str> = format!("mcp-{}", engram_core::id::new_id()).into();
        Self {
            _trace: SessionTrace::start(&engine, &session_id, None),
            engine,
            bound: None,
            conflicts: Arc::new(Mutex::new(hub.subscribe_conflicts())),
            hub,
            session_id,
        }
    }

    /// Bound to one project of the hub (v0.6.2 machine core): `selector`'s
    /// engine is this session's current project — what an omitted `project`
    /// param means — regardless of which project the core launched with.
    pub fn for_project(hub: Arc<Hub>, selector: &str) -> Result<Self, engram_core::Error> {
        let engine = hub.get(selector)?;
        let session_id: Arc<str> = format!("mcp-{}", engram_core::id::new_id()).into();
        Ok(Self {
            _trace: SessionTrace::start(&engine, &session_id, Some(format!("project {selector}"))),
            engine,
            bound: Some(selector.into()),
            conflicts: Arc::new(Mutex::new(hub.subscribe_conflicts())),
            hub,
            session_id,
        })
    }

    /// Lock the engine and stamp this MCP session as the writer (audit journal
    /// attribution). Re-stamped on every operation: the engine may be shared
    /// with the HTTP pane, which stamps itself the same way.
    fn engine(&self) -> std::sync::MutexGuard<'_, Engine> {
        let mut guard = self.engine.lock().unwrap();
        guard.set_audit_origin(engram_core::AuditOrigin::mcp(self.session_id.to_string()));
        guard
    }

    /// Resolve a tool's optional `project` selector: omitted = the current
    /// project, a name/id = that registered project, `home` = the user-level
    /// home graph. `all` never resolves to one engine — the hub's refusal
    /// explains where fan-out reads and shared writes belong.
    fn engine_for(&self, project: &Option<String>) -> Result<Arc<Mutex<Engine>>, ErrorData> {
        match project.as_deref() {
            None => Ok(self.engine.clone()),
            Some(sel) => self.hub.get(sel).map_err(map_err),
        }
    }

    /// Apply a search `detail` level: compact strips snippets and neighbors,
    /// snippet is the wire shape as-is, full attaches each hit's body.
    fn shape_hits(
        &self,
        detail: &Detail,
        hits: Vec<engram_core::SearchHit>,
        engine: Option<&Arc<Mutex<Engine>>>,
    ) -> serde_json::Value {
        match detail {
            Detail::Snippet => json!(hits),
            Detail::Compact => json!(
                hits.iter()
                    .map(|h| {
                        let mut o = json!({
                            "id": h.id,
                            "type": h.node_type,
                            "title": h.title,
                            "score": h.score,
                            "trust": h.trust,
                            "stale": h.stale,
                        });
                        if let Some(p) = &h.project {
                            o["project"] = json!(p);
                        }
                        if let Some(s) = h.status {
                            o["status"] = json!(s);
                        }
                        o
                    })
                    .collect::<Vec<_>>()
            ),
            Detail::Full => json!(
                hits.iter()
                    .map(|h| {
                        let mut o = json!(h);
                        let resolved = match (&h.project, engine) {
                            (Some(p), _) => self.hub.get(p).ok(),
                            (None, Some(e)) => Some((*e).clone()),
                            (None, None) => Some(self.engine.clone()),
                        };
                        if let Some(e) = resolved
                            && let Ok(Some(node)) = e.lock().unwrap().get_node(&h.id)
                        {
                            o["body"] = json!(node.body);
                            o["tags"] = json!(node.tags);
                            o["code_refs"] = json!(node.code_refs);
                        }
                        o
                    })
                    .collect::<Vec<_>>()
            ),
        }
    }

    /// The mid-session conflict push, delivery side: whatever judged
    /// contradictions landed since this session's last tool call, formatted
    /// as one out-of-band text block. Titles are resolved here — at delivery
    /// time, with short engine locks — never inside the emitting listener.
    fn drain_conflict_alerts(&self) -> Option<String> {
        let alerts = self.conflicts.lock().unwrap().drain();
        if alerts.is_empty() {
            return None;
        }
        let title_of = |project: &str, id: &str| -> String {
            self.hub
                .get(project)
                .ok()
                .and_then(|e| e.lock().unwrap().get_node(id).ok().flatten())
                .map(|n| n.title)
                .unwrap_or_else(|| id.to_string())
        };
        let mut lines = vec![format!(
            "MEMORY ALERT: {} contradiction(s) were judged while you worked — check whether they touch your current task before relying on either side.",
            alerts.len()
        )];
        for a in alerts.iter().take(3) {
            lines.push(format!(
                "- \"{}\" now conflicts-with \"{}\" [project {}, edge {}] — get_node either side for the full claims.",
                title_of(&a.project, &a.from_id),
                title_of(&a.project, &a.to_id),
                a.project,
                a.edge_id
            ));
        }
        if alerts.len() > 3 {
            lines.push(format!(
                "- (+{} more — see list_suspects / the pane Review drawer)",
                alerts.len() - 3
            ));
        }
        Some(lines.join("\n"))
    }

    /// The standard tool reply: the JSON payload, plus any pending conflict
    /// alert as a second content block — every tool call is a delivery
    /// window, so the push needs no client or bridge support at all.
    fn reply<T: Serialize>(&self, v: &T) -> Result<CallToolResult, ErrorData> {
        let mut result = ok_json(v)?;
        if let Some(alert) = self.drain_conflict_alerts() {
            result.content.push(ContentBlock::text(alert));
        }
        Ok(result)
    }

    /// Decorate a write response with the canon check's verdicts and the
    /// action note they call for (shared by add_note and update_node).
    fn attach_canon(out: &mut serde_json::Value, canon: &[engram_core::CanonVerdict]) {
        if canon.is_empty() {
            return;
        }
        out["canon"] = json!(canon);
        out["canon_note"] = json!(if canon.iter().any(|c| c.verdict == "contradicts") {
            "existing canon CONTRADICTS this text (see `canon`) — read the flagged node; \
             if the disagreement is real, link conflicts-with and tell the user"
        } else {
            "existing canon already supports this text (see `canon`) — consider linking it \
             (because / builds-on) instead of leaving the reinforcement implicit"
        });
    }

    /// Lock a scoped engine with this session stamped as the writer.
    fn mcp<'a>(&self, engine: &'a Arc<Mutex<Engine>>) -> std::sync::MutexGuard<'a, Engine> {
        let mut guard = engine.lock().unwrap();
        guard.set_audit_origin(engram_core::AuditOrigin::mcp(self.session_id.to_string()));
        guard
    }

    #[tool(
        description = "Hybrid semantic + keyword search over the memory graph. \
        Hits carry: type, title, snippet, score, trust (computed 0..1), stale \
        (true = decayed trust, verify before relying), status, and 1-hop \
        neighbors (conflicts-with/replaces first). The reply's `confidence` \
        is a verdict to respect: `strong` = the top hit cleared the \
        calibrated line; `weak` = likely not in memory — the hits are the \
        nearest candidates (never cut), verify before relying on one; \
        `none` = the graph is silent — say so instead of inventing a \
        memory. The line is calibrated per graph by auto-tune. Weak-scoring \
        tail hits are trimmed before delivery so \
        attention stays on the answer. Being returned stamps \
        last_seen for observability only — retrieval never refreshes trust. \
        SCOPE IT IN TIME when the question is temporal: `after`/`before` take \
        a day, an ISO instant, or a relative expression the daemon resolves \
        (\"yesterday\", \"last week\", \"last 3 days\", \"2 hours ago\", \"this \
        year\") — don't compute dates yourself; `during_version` (e.g. \
        \"0.8.4\") scopes to when that working version was current. \
        `order: \"chronological\"` reads oldest-first for how something \
        developed, `order: \"recent\"` newest-first for the CURRENT value of \
        something that changed. The window filters before the confidence \
        verdict, so a scoped verdict is about the scoped set. \
        `project: \"all\"` searches every registered project plus the home \
        graph — foreign hits carry `project` provenance and rank under a \
        locality prior, so the local canon wins ties."
    )]
    async fn search(
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
        // One grammar for both layers (0.8.7): the filter is resolved once,
        // against the graph's own version journal, and then applies to
        // whichever layer the scope selects.
        let filter = {
            let guard = engine.lock().unwrap();
            guard
                .time_filter(
                    a.after.as_deref(),
                    a.before.as_deref(),
                    a.during_version.as_deref(),
                    a.order.as_deref(),
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
        let (mut hits, confidence) = {
            let guard = self.mcp(&engine);
            let hits = guard
                .search_filtered(&a.query, &types, limit, &filter)
                .map_err(map_err)?;
            let confidence = guard.search_confidence(&hits);
            (hits, confidence)
        };
        hits.iter_mut().for_each(debracket);
        let hit_ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
        let mut hits = self.shape_hits(&detail, hits, Some(&engine));
        // Provenance line (0.8.4): a curated hit born in a recorded exchange
        // says so — the birth dialogue is one expand_history away.
        if let Some(arr) = hits.as_array_mut() {
            let guard = engine.lock().unwrap();
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
        if scope == "auto" && matches!(confidence, Some("weak") | Some("none")) {
            let guard = engine.lock().unwrap();
            if guard.config().history.search_fallthrough && guard.history_open() {
                drop(guard);
                let history = {
                    let guard = self.mcp(&engine);
                    guard
                        .search_history_filtered(&a.query, limit, &filter)
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
        self.reply(&body)
    }

    #[tool(
        description = "Read the recorded exchange around one turn of a session-history hit: \
        the surrounding user/assistant dialogue, in order. Takes the `session` and `turn` \
        handles a history hit (or a curated hit's born_in line) carried; `window` messages \
        of context each side (default 4). History is raw dialogue, not curated memory. \
        The reply also carries `notes`: every curated memory note born during this \
        session (its born-in provenance, reversed), each one a get_node away."
    )]
    async fn expand_history(
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
        description = "Browse the recorded session history: every session, newest first — \
        title (the opening user message), harness, start/end, message count, and the \
        `session` handle expand_history takes. The browsing entry point when you don't \
        have a search hit to start from; empty when recording is off. Scope it in time \
        with `after`/`before` (a day, an ISO instant, or \"yesterday\" / \"last week\" / \
        \"3 days ago\") to answer \"what was I working on then\" without needing a search \
        hit at all. History is raw dialogue records, not curated memory."
    )]
    async fn list_sessions(
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
        description = "Fetch one node by id with its outgoing and incoming edges. \
        Node fields include computed trust (0..1) and stale (true = trust < 0.3). \
        Optional `parents`/`children` (depth 0-3) also return the reasoning \
        hierarchy: parents are nodes this one points at (its reasons/subjects — \
        e.g. the Principle behind a Decision); children are nodes pointing at it \
        (what answers / builds on it). Nested as {edge, node, parents|children}. \
        A node born in a recorded exchange carries `born_in` (session, turn) — \
        the birth dialogue is one expand_history away."
    )]
    async fn get_node(
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
        // Provenance (0.8.4): same line search hits carry — where this note
        // was born, when the history layer recorded the exchange.
        if let Some(born) = engine.born_in_of(&a.id) {
            payload["born_in"] = json!(born);
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
    async fn traverse(
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
        description = "Create a memory node (source = claude, starts provisional). \
        ALWAYS read the response, it is a verdict, not a receipt — every check runs in this \
        same turn: {matched, created: false} = a same-type near-duplicate exists, merge via \
        update_node (if it carries nli_label=contradiction it is a NEGATED duplicate — read \
        before merging, likely a conflicts-with instead); `warnings` = the note landed near \
        contradicted or superseded knowledge; `missing_code_refs` = paths that don't resolve \
        in the repo, fix or drop them; `suspects` = queued look-alike pairs (each may carry \
        an nli hint), judge each with resolve_suspect now and tell the user if one is a \
        genuine contradiction."
    )]
    async fn add_note(
        &self,
        Parameters(a): Parameters<AddNoteArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let payload = self.create_note(a)?;
        self.reply(&payload)
    }

    /// The add_note core, shared with the batch form. Each note resolves its
    /// own `project` — a write addressed to `all` is refused by the hub with
    /// the home-graph pointer (PLAN §7C: fan-out writes are replication).
    fn create_note(&self, a: AddNoteArgs) -> Result<serde_json::Value, ErrorData> {
        let node_type = NodeType::parse(&a.node_type).map_err(map_err)?;
        let engine = self.engine_for(&a.project)?;
        // Durability defaults from the ontology; born-open status for
        // worklist types is applied by Engine::add_node (the write boundary
        // owns it, so every surface gets it).
        let durability = match a.durability {
            Some(d) => Durability::parse(&d).map_err(map_err)?,
            None => engine.lock().unwrap().default_durability(&node_type),
        };
        let created_at = match a.created_at.as_deref() {
            None => None,
            Some(raw) => Some(engram_core::parse_day(raw).ok_or_else(|| {
                ErrorData::invalid_params(
                    format!("created_at {raw:?} is not YYYY-MM-DD or unix seconds"),
                    None,
                )
            })?),
        };
        let outcome = self
            .mcp(&engine)
            .add_node_checked(NewNode {
                node_type,
                title: a.title,
                body: a.body,
                created_at,
                durability,
                source: Source::Claude,
                session_id: a.session_id.or_else(|| Some(self.session_id.to_string())),
                status: None,
                code_refs: a.code_refs,
                tags: a.tags,
                version: a.version,
                props: None,
            })
            .map_err(map_err)?;
        Ok(match outcome {
            WriteOutcome::Created {
                node,
                warnings,
                suspects,
                missing_refs,
                canon,
            } => {
                // born-in provenance (0.8.4): a live MCP write was born in
                // whatever exchange is happening right now — park it; the
                // harvester links it once the transcript catches up.
                engine
                    .lock()
                    .unwrap()
                    .park_provenance(&node.id, node.created_at);
                let mut out = json!({ "id": node.id, "created": true });
                if !warnings.is_empty() {
                    out["warnings"] = json!(warnings);
                }
                if !missing_refs.is_empty() {
                    out["missing_code_refs"] = json!(missing_refs);
                    out["refs_note"] = json!(
                        "these code_refs don't resolve in the repo right now — fix the paths or drop them"
                    );
                }
                if !suspects.is_empty() {
                    out["suspects"] = json!(suspects);
                    out["action_required"] = json!(SUSPECT_ACTION);
                }
                Self::attach_canon(&mut out, &canon);
                out
            }
            WriteOutcome::Matched {
                node,
                similarity,
                nli_label,
                nli_score,
            } => {
                let mut out = json!({
                    "matched": node.id,
                    "created": false,
                    "title": node.title,
                    "similarity": similarity,
                });
                if let (Some(label), Some(score)) = (&nli_label, nli_score) {
                    out["nli_label"] = json!(label);
                    out["nli_score"] = json!(score);
                    if label == "contradiction" {
                        out["action_required"] = json!(
                            "The near-duplicate may CONTRADICT your text (negated duplicate — \
                             'use X' vs 'don't use X'). Read the matched node before merging; \
                             if it genuinely disagrees, capture yours as a new node and link \
                             conflicts-with instead of updating the match."
                        );
                    }
                }
                out
            }
        })
    }

    #[tool(
        description = "Batch create: add several notes in one call — each item \
        runs the same near-duplicate pre-check and redaction as add_note. \
        Results are per-item and positional: {id, created} | {matched, \
        created: false} | {ok: false, error}; one bad item never blocks the \
        rest. For seeding passes and multi-note stopping points."
    )]
    async fn add_notes(
        &self,
        Parameters(a): Parameters<AddNotesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if a.notes.len() > BATCH_CAP {
            return Err(ErrorData::invalid_params(
                format!("at most {BATCH_CAP} notes per call"),
                None,
            ));
        }
        let results: Vec<serde_json::Value> = a
            .notes
            .into_iter()
            .map(|item| {
                self.create_note(item)
                    .unwrap_or_else(|e| json!({ "ok": false, "error": e.message }))
            })
            .collect();
        self.reply(&json!({ "results": results }))
    }

    #[tool(
        description = "Session-start digest of the memory graph as markdown: unresolved \
        conflicts, open problems/intents, principles, decisions, cautions, recent changes \
        — token-budgeted. Call this once when starting work on the project."
    )]
    async fn brief(
        &self,
        Parameters(a): Parameters<BriefArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // No explicit budget → the briefed graph's configured one.
        let max_chars = self
            .engine_for(&a.project)?
            .lock()
            .unwrap()
            .brief_chars(a.max_chars);
        // Unscoped = THIS SESSION's project plus the home-graph section; a
        // scoped project (or `home`) briefs that graph alone. The session's
        // binding is the whole point: the hub's own current project is the
        // core's launch graph (home), which is nobody's answer.
        let text = match &a.project {
            None => self
                .hub
                .brief_for(self.bound.as_deref(), max_chars)
                .map_err(map_err)?,
            Some(_) => {
                let engine = self.engine_for(&a.project)?;
                self.mcp(&engine).brief(max_chars).map_err(map_err)?
            }
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(description = "Describe this graph's ontology: every node type (the \
        thought it captures, default durability, roles) and every edge verb \
        (worked example, roles). The graph defines its own ontology (it may \
        be customized per project) — call this when type or verb names \
        surprise you, or before writing into an unfamiliar graph.")]
    async fn describe_ontology(
        &self,
        Parameters(a): Parameters<DescribeOntologyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let text = engine.lock().unwrap().config().describe_ontology();
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(description = "Set (or clear, with null) the graph's CURRENT WORKING \
        VERSION — version tracking stamps it on every new version-bound note \
        so the graph shows when each piece of knowledge was captured. Call it \
        when the project moves to a new version (release cut, version bump). \
        Setting a version turns version tracking on if it was off (clearing \
        never does). The response carries the recent switch history.")]
    async fn set_version(
        &self,
        Parameters(a): Parameters<SetVersionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let (previous, history, enabled_by_this_call) = {
            let engine = self.mcp(&engine);
            let was_on = engine.config().versioning.enabled;
            let previous = engine
                .set_current_version(a.version.as_deref())
                .map_err(map_err)?;
            let history: Vec<String> = engine
                .audit_log(None, Some("version"), 10)
                .map_err(map_err)?
                .entries
                .into_iter()
                .filter_map(|r| r.title)
                .collect();
            (previous, history, a.version.is_some() && !was_on)
        };
        let mut reply = json!({
            "ok": true,
            "previous": previous,
            "current": a.version,
            "history": history,
        });
        if enabled_by_this_call {
            reply["versioning"] = json!(
                "was off — enabled by this call; new version-bound notes are stamped from now on"
            );
        }
        self.reply(&reply)
    }

    #[tool(description = "Delete one edge by id — for repairing a mislink. \
        Nodes are never deleted this way (hard node delete is user-only).")]
    async fn unlink(&self, Parameters(a): Parameters<IdArg>) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let removed = self.mcp(&engine).delete_edge(&a.id).map_err(map_err)?;
        if !removed {
            return Err(ErrorData::invalid_params(
                format!("edge not found: {}", a.id),
                None,
            ));
        }
        self.reply(&json!({ "ok": true }))
    }

    #[tool(
        description = "Update an edge's status (active | resolved | dismissed), \
        note, or confidence — e.g. mark a conflicts-with as resolved."
    )]
    async fn update_edge(
        &self,
        Parameters(a): Parameters<UpdateEdgeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let patch = EdgePatch {
            // Retype stays a pane action; Claude repairs a wrong verb with
            // unlink + link (which re-states the sentence deliberately).
            edge_type: None,
            status: a
                .status
                .map(|s| EdgeStatus::parse(&s))
                .transpose()
                .map_err(map_err)?,
            note: a.note,
            confidence: a.confidence,
            strength: None,
        };
        let engine = self.engine_for(&a.project)?;
        let edge = self
            .mcp(&engine)
            .update_edge(&a.id, patch)
            .map_err(map_err)?;
        self.reply(&json!({ "ok": true, "id": edge.id }))
    }

    #[tool(description = "Link two nodes with a sentence-shaped edge \
        (about, because, answers, builds-on, replaces, conflicts-with, needs).")]
    async fn link(&self, Parameters(a): Parameters<LinkArgs>) -> Result<CallToolResult, ErrorData> {
        let edge_type = EdgeType::parse(&a.edge_type).map_err(map_err)?;
        let engine = self.engine_for(&a.project)?;
        let edge = self
            .mcp(&engine)
            .add_edge(NewEdge {
                edge_type,
                from_id: a.from,
                to_id: a.to,
                source: Source::Claude,
                note: a.note,
                confidence: a.confidence,
                strength: None,
                status: None,
            })
            .map_err(map_err)?;
        let contradiction =
            engine.lock().unwrap().config().contradiction_verb() == edge.edge_type.as_str();
        if contradiction {
            // This session just recorded the contradiction deliberately — it
            // already knows; don't echo its own alert back on the next call.
            let _ = self.drain_conflict_alerts();
        }
        self.reply(&json!({ "id": edge.id }))
    }

    #[tool(
        description = "Verify a claim against the memory graph using the local \
        NLI model: returns {supports, contradicts, silent} — nodes that entail \
        the claim, nodes that contradict it, and nearby nodes with no verdict. \
        Use before acting on an assumption ('does the canon contradict this \
        plan?'). Contradicts-hits are conflicts to surface; all-silent on a \
        real topic is a gap worth capturing. Verdicts are hints from a small \
        local model — judgment stays with you."
    )]
    async fn check_claim(
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
        description = "Pending suspected conflicts from the local scan: unlinked \
        look-alike node pairs awaiting judgment (each may carry an nli_label / \
        nli_score triage hint from the local model — a suggestion, not a \
        verdict). Judge each with resolve_suspect."
    )]
    async fn list_suspects(
        &self,
        Parameters(a): Parameters<ProjectArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let suspects = self.mcp(&engine).suspects().map_err(map_err)?;
        self.reply(&json!({ "suspects": suspects }))
    }

    #[tool(
        description = "Judge a suspected conflict: verdict `conflict` records a \
        conflicts-with edge, `replaces` records a replaces edge AND archives the \
        older node, `dismiss` marks the pair fine-together (never re-raised)."
    )]
    async fn resolve_suspect(
        &self,
        Parameters(a): Parameters<ResolveSuspectArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let verdict = SuspectVerdict::parse(&a.verdict).map_err(map_err)?;
        let engine = self.engine_for(&a.project)?;
        let edge = self
            .mcp(&engine)
            .resolve_suspect(&a.id, verdict, Source::Claude)
            .map_err(map_err)?;
        if matches!(verdict, SuspectVerdict::Conflict) {
            // The judge doesn't need its own verdict pushed back at it.
            let _ = self.drain_conflict_alerts();
        }
        self.reply(&json!({ "ok": true, "edge": edge }))
    }

    #[tool(description = "Approve a node: trust restarts at 100% (and holds \
        there on stable knowledge until contradicting evidence lands). ONLY on \
        explicit user demand, or after verifying the node's content \
        word-by-word against current reality. Routine still-relevant signals \
        belong in update_node, not here.")]
    async fn approve_node(
        &self,
        Parameters(a): Parameters<ApproveArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let node = self.mcp(&engine).approve(&a.id).map_err(map_err)?;
        self.reply(&json!({ "ok": true, "id": node.id, "trust": node.trust }))
    }

    #[tool(
        description = "Update fields on an existing node (merge / reclassify / \
        confirm still true). A deliberate update stamps confirmed_at — the \
        unapproved trust anchor — and clears any evidence demotion; this is \
        how a verified-still-true stale node gets its trust back. Re-embeds \
        when any indexed field changes. Read the response like add_note's: \
        `warnings` and `suspects` carry the same act-now duties (judge \
        suspects via resolve_suspect; surface real contradictions to the user)."
    )]
    async fn update_node(
        &self,
        Parameters(a): Parameters<UpdateArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let payload = self.patch_node(a)?;
        self.reply(&payload)
    }

    /// The update_node core, shared with the batch form.
    fn patch_node(&self, a: UpdateArgs) -> Result<serde_json::Value, ErrorData> {
        let patch = NodePatch {
            version: a.version,
            node_type: a
                .node_type
                .map(|t| NodeType::parse(&t))
                .transpose()
                .map_err(map_err)?,
            title: a.title,
            body: a.body,
            durability: a
                .durability
                .map(|d| Durability::parse(&d))
                .transpose()
                .map_err(map_err)?,
            status: a
                .status
                .map(|s| NodeStatus::parse(&s))
                .transpose()
                .map_err(map_err)?,
            valid_until: None,
            code_refs: a.code_refs,
            tags: a.tags,
        };
        let engine = self.engine_for(&a.project)?;
        let engram_core::CheckedUpdate {
            node,
            warnings,
            suspects,
            missing_refs,
            canon,
        } = self
            .mcp(&engine)
            .update_node_checked(&a.id, patch)
            .map_err(map_err)?;
        let mut out = json!({ "ok": true, "id": node.id });
        if !warnings.is_empty() {
            out["warnings"] = json!(warnings);
        }
        if !missing_refs.is_empty() {
            out["missing_code_refs"] = json!(missing_refs);
            out["refs_note"] = json!(
                "these code_refs don't resolve in the repo right now — fix the paths or drop them"
            );
        }
        if !suspects.is_empty() {
            out["suspects"] = json!(suspects);
            out["action_required"] = json!(SUSPECT_ACTION);
        }
        Self::attach_canon(&mut out, &canon);
        Ok(out)
    }

    #[tool(description = "Merge duplicate notes into one survivor: unions tags \
        and code_refs, rehomes the victims' live edges onto the survivor \
        (deduped; self-loops and incoming replaces stay behind as the \
        victim's story), writes a replaces edge per victim so each remains \
        traversable as an archived generation, and stamps the survivor \
        confirmed. Supersession, not deletion — nothing is destroyed. Use \
        when several notes state the same knowledge; pass title/body with \
        the merged text you composed from the parts (omitted = the \
        survivor's text stands, which silently drops the victims' content). \
        Refused when a victim is user-pinned — surface that to the user. \
        Read the response like add_note's: `warnings` and `suspects` carry \
        the same act-now duties.")]
    async fn merge_nodes(
        &self,
        Parameters(a): Parameters<MergeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if a.victims.len() > BATCH_CAP {
            return Err(ErrorData::invalid_params(
                format!("at most {BATCH_CAP} victims per call"),
                None,
            ));
        }
        let engine = self.engine_for(&a.project)?;
        let outcome = self
            .mcp(&engine)
            .merge_nodes(&a.survivor, &a.victims, a.title, a.body, Source::Claude)
            .map_err(map_err)?;
        let mut out = json!({
            "ok": true,
            "survivor": outcome.survivor.id,
            "merged": outcome.merged,
        });
        if !outcome.warnings.is_empty() {
            out["warnings"] = json!(outcome.warnings);
        }
        if !outcome.missing_refs.is_empty() {
            out["missing_code_refs"] = json!(outcome.missing_refs);
        }
        if !outcome.suspects.is_empty() {
            out["suspects"] = json!(outcome.suspects);
            out["action_required"] = json!(SUSPECT_ACTION);
        }
        Self::attach_canon(&mut out, &outcome.canon);
        self.reply(&out)
    }

    #[tool(
        description = "Batch update: apply several node patches in one call — \
        the bulk counterpart of update_node for curation sweeps (term renames, \
        status fixes, tag hygiene). Each item takes the same fields as \
        update_node; items apply independently, results are positional \
        ({ok, id} | {ok: false, id, error}), one bad item never blocks the \
        rest, and every change lands in the audit journal individually."
    )]
    async fn update_nodes(
        &self,
        Parameters(a): Parameters<UpdateNodesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if a.updates.len() > BATCH_CAP {
            return Err(ErrorData::invalid_params(
                format!("at most {BATCH_CAP} updates per call"),
                None,
            ));
        }
        let results: Vec<serde_json::Value> = a
            .updates
            .into_iter()
            .map(|item| {
                let id = item.id.clone();
                self.patch_node(item)
                    .unwrap_or_else(|e| json!({ "ok": false, "id": id, "error": e.message }))
            })
            .collect();
        self.reply(&json!({ "results": results }))
    }

    #[tool(description = "Full-fidelity paged read of the graph: complete nodes \
        (whole body, tags, status, durability, code_refs, computed trust) with \
        optional filters — types, status, tag, include_archived, pinned \
        (pinned: true reads the user's constant-trust canon). This is the \
        lossless bulk read for reviews and exports: building a decisions.md \
        means paging every Decision with its full body, which search snippets \
        and the budgeted brief cannot provide. Newest first; `total` is the \
        filtered count, page with limit/offset. Read-only — does not refresh \
        trust clocks.")]
    async fn list_nodes(
        &self,
        Parameters(a): Parameters<ListNodesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let types = node_types(&a.types)?;
        let status = a
            .status
            .map(|s| NodeStatus::parse(&s))
            .transpose()
            .map_err(map_err)?;
        let tag = a
            .tag
            .map(|t| engram_core::normalize_tags(&[t]))
            .and_then(|mut v| v.pop());
        let engine = self.engine_for(&a.project)?;
        let (mut nodes, _) = self.mcp(&engine).graph().map_err(map_err)?;
        nodes.retain(|n| {
            (a.include_archived.unwrap_or(false) || n.valid_until.is_none())
                && (types.is_empty() || types.contains(&n.node_type))
                && status.is_none_or(|s| n.status == Some(s))
                && tag.as_ref().is_none_or(|t| n.tags.contains(t))
                && a.pinned.is_none_or(|p| n.trust_override.is_some() == p)
        });
        match a.sort.as_deref() {
            None | Some("recent") => {
                // Ids are time-sortable, so this is newest-first creation order.
                nodes.sort_by(|x, y| y.id.cmp(&x.id));
            }
            Some(order @ ("most-connected" | "least-connected")) => {
                // Degree over live edges: the least-connected end surfaces
                // reachability islands (no links means text search is the
                // only road in), the most-connected end surfaces the hubs.
                let mut degree: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for e in self.mcp(&engine).store().all_edges().map_err(map_err)? {
                    *degree.entry(e.from_id).or_default() += 1;
                    *degree.entry(e.to_id).or_default() += 1;
                }
                nodes.sort_by(|x, y| {
                    let (dx, dy) = (
                        degree.get(&x.id).copied().unwrap_or(0),
                        degree.get(&y.id).copied().unwrap_or(0),
                    );
                    match order {
                        "most-connected" => dy.cmp(&dx).then(y.id.cmp(&x.id)),
                        _ => dx.cmp(&dy).then(y.id.cmp(&x.id)),
                    }
                });
            }
            Some(other) => {
                return Err(ErrorData::invalid_params(
                    format!("sort {other:?}: use recent | most-connected | least-connected"),
                    None,
                ));
            }
        }
        let total = nodes.len();
        let offset = a.offset.unwrap_or(0);
        let limit = a.limit.unwrap_or(30).min(200);
        let page: Vec<Node> = nodes.into_iter().skip(offset).take(limit).collect();
        self.reply(&json!({ "total": total, "offset": offset, "nodes": page }))
    }

    #[tool(description = "List the live worklist: open Problems and Intents.")]
    async fn list_open(
        &self,
        Parameters(a): Parameters<ListOpenArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let types = node_types(&a.types)?;
        let engine = self.engine_for(&a.project)?;
        let nodes = self
            .mcp(&engine)
            .worklist(&types, a.include_conflicts.unwrap_or(true))
            .map_err(map_err)?;
        self.reply(&nodes)
    }

    #[tool(
        description = "The chronological story of one piece of knowledge: the \
        node's `replaces` chain, oldest first. Each superseded generation \
        carries the note of the replaces edge that retired it. Use to answer \
        \"how did this decision evolve\"."
    )]
    async fn timeline(
        &self,
        Parameters(a): Parameters<IdArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let chain = self.mcp(&engine).timeline(&a.id).map_err(map_err)?;
        self.reply(&json!({ "timeline": chain }))
    }

    #[tool(description = "Nodes whose path-shaped code_refs no longer exist in \
        the project — the code moved and the memory didn't (drifted). Review \
        each: fix the refs via update_node, and check whether the knowledge \
        itself is still true (supersede or conflicts-with it if not).")]
    async fn list_drift(
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

    #[tool(description = "One page of the audit journal, newest first: every \
        node/edge mutation with before/after snapshots and writer context \
        (origin, session, cwd, pid, version). Filter to one node/edge with \
        entity_id; page with before = the last row's seq. Read-only — answers \
        \"what changed while I was away\" and \"who wrote this\".")]
    async fn audit(
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

    #[tool(description = "Every project this memory hub can reach: the current \
        project, the user-level home graph, and the machine registry \
        (~/.engram/registry.json — populated by every engram-alpha serve/mcp \
        run). Use the names here as the `project` argument other tools accept: \
        omit = current, a name = that project (reads AND writes), 'home' = \
        the shared user-level graph, 'all' = fan a search/check_claim out \
        across everything (reads only).")]
    async fn list_projects(&self) -> Result<CallToolResult, ErrorData> {
        // `current` is THIS SESSION's project, not the core's launch graph —
        // a bound session told it is in `home` reads as the wrong graph
        // being open and sends the assistant looking for its memory elsewhere.
        self.reply(&json!({ "projects": self.hub.projects_for(self.bound.as_deref()) }))
    }
}

/// How many concrete node resources `resources/list` advertises (newest
/// first); the full graph stays reachable through the uri template.
const RESOURCE_LIST_CAP: usize = 25;

/// The identity every engram MCP endpoint advertises. Shared by the real
/// server and the roots-mode bridge, which must answer the stdio initialize
/// before its upstream exists — same crate, same contract.
fn engram_server_info() -> ServerInfo {
    ServerInfo::new(
        ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build(),
    )
    .with_server_info(Implementation::new("engram", env!("CARGO_PKG_VERSION")))
    .with_instructions(INSTRUCTIONS.to_string())
}

#[tool_handler]
impl ServerHandler for Engram {
    fn get_info(&self) -> ServerInfo {
        engram_server_info()
    }

    /// Appendix A: `engram://node/{id}` so a user can @-mention a node in a
    /// prompt. The list shows the newest nodes; anything else resolves
    /// through the template with an id from `search`/the pane.
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let nodes = self
            .engine()
            .store()
            .recent_nodes(RESOURCE_LIST_CAP)
            .map_err(map_err)?;
        Ok(ListResourcesResult {
            meta: None,
            next_cursor: None,
            resources: nodes
                .into_iter()
                .map(|n| {
                    Resource::new(format!("engram://node/{}", n.id), n.id.clone())
                        .with_title(n.title)
                        .with_description(format!(
                            "{} node in the Engram memory graph",
                            n.node_type.as_str()
                        ))
                        .with_mime_type("application/json")
                })
                .collect(),
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult {
            meta: None,
            next_cursor: None,
            resource_templates: vec![
                ResourceTemplate::new("engram://node/{id}", "node")
                    .with_title("Engram memory node")
                    .with_description(
                        "One memory node with its edges, by id (ids come from search, \
                         the brief, or the pane)",
                    )
                    .with_mime_type("application/json"),
            ],
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let Some(id) = request.uri.strip_prefix("engram://node/") else {
            return Err(ErrorData::invalid_params(
                format!("unknown resource uri: {}", request.uri),
                None,
            ));
        };
        let engine = self.engine();
        let Some(node) = engine.get_node(id).map_err(map_err)? else {
            return Err(ErrorData::invalid_params(
                format!("node not found: {id}"),
                None,
            ));
        };
        let out = engine.edges_out(id).map_err(map_err)?;
        let incoming = engine.edges_in(id).map_err(map_err)?;
        let payload = json!({ "node": node, "edges_out": out, "edges_in": incoming });
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(payload.to_string(), request.uri)
                .with_mime_type("application/json"),
        ]))
    }
}

/// Progressive disclosure for search results (v0.6.3): compact → snippet →
/// full, so the assistant starts cheap and expands on demand.
enum Detail {
    Compact,
    Snippet,
    Full,
}

impl Detail {
    fn parse(s: Option<&str>) -> Result<Self, ErrorData> {
        match s {
            None | Some("snippet") => Ok(Detail::Snippet),
            Some("compact") => Ok(Detail::Compact),
            Some("full") => Ok(Detail::Full),
            Some(other) => Err(ErrorData::invalid_params(
                format!("detail {other:?}: use compact | snippet | full"),
                None,
            )),
        }
    }
}

/// Serve the MCP protocol over stdio until the client disconnects.
pub async fn serve_stdio(engine: Engine) -> anyhow::Result<()> {
    serve(Engram::new(engine)).await
}

/// Serve over stdio using an engine shared with the HTTP server.
pub async fn serve_stdio_shared(engine: Arc<Mutex<Engine>>) -> anyhow::Result<()> {
    serve(Engram::with_shared(engine)).await
}

async fn serve(server: Engram) -> anyhow::Result<()> {
    let service = server.serve(rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

// ---- daemon-hosted MCP (PLAN §0 transport migration / §7C thin clients) ----

/// The daemon-hosted MCP service type, nameable so the CLI can cache one per
/// project (v0.6.2: `/projects/{id}/mcp`).
pub type McpHttpService = rmcp::transport::StreamableHttpService<
    Engram,
    rmcp::transport::streamable_http_server::session::local::LocalSessionManager,
>;

/// The daemon's `/mcp` endpoint: MCP over streamable HTTP as a tower service
/// for the daemon router. Stateful — each connected client becomes one
/// session with its own [`Engram`] instance over the shared hub, so
/// per-session audit attribution works exactly as one stdio process did.
pub fn streamable_http_service(hub: Arc<Hub>) -> McpHttpService {
    rmcp::transport::StreamableHttpService::new(
        move || Ok(Engram::with_hub(hub.clone())),
        Arc::new(Default::default()),
        Default::default(),
    )
}

/// The per-project form (v0.6.2 machine core): sessions on this service
/// treat `selector`'s graph as the current project — an MCP bridge from repo
/// X binds to X, however the core was launched. The `project` tool param
/// still overrides per call.
pub fn streamable_http_service_for(hub: Arc<Hub>, selector: String) -> McpHttpService {
    rmcp::transport::StreamableHttpService::new(
        move || {
            Engram::for_project(hub.clone(), &selector)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()))
        },
        Arc::new(Default::default()),
        Default::default(),
    )
}

/// The stdio side of the thin client: an MCP passthrough from a stdio client
/// (Claude Code and friends launch us this way) to a core's per-project MCP
/// endpoint. Exists because redb allows one process per store — the core
/// holds the file; everything else, including this bridge, talks HTTP.
///
/// Since the roots Decision (0.8.8) the bridge has exactly ONE bounded brain:
/// deciding WHICH project a session binds to. With an explicit `--db` the
/// upstream is fixed and the proxy stays verbatim (the pre-roots doctrine);
/// without one, `roots` is set and the binding resolves from the client's MCP
/// roots (first `file://` root, falling back to the bridge's cwd) and follows
/// `notifications/roots/list_changed` across project switches — one global
/// config entry serves every project (the Windsurf case, issue #4).
struct Passthrough {
    state: Arc<BridgeState>,
    info: rmcp::model::ServerInfo,
    roots: Option<Arc<RootsBinding>>,
}

fn proxy_err(e: rmcp::ServiceError) -> ErrorData {
    match e {
        rmcp::ServiceError::McpError(data) => data,
        other => ErrorData::internal_error(format!("daemon bridge: {other}"), None),
    }
}

impl rmcp::Service<rmcp::RoleServer> for Passthrough {
    async fn handle_request(
        &self,
        request: rmcp::model::ClientRequest,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ServerResult, ErrorData> {
        // The initialize is answered from `info` directly — the upstream
        // doesn't exist yet in either mode (handshake-first): Fixed is
        // still resolving its core in the background, and Roots binding
        // starts only after this handshake tells us whether the client
        // speaks roots.
        if let rmcp::model::ClientRequest::InitializeRequest(init) = &request {
            // Census legibility: remember WHO this client is
            // (clientInfo.name — "claude-code", "mcp-go", …) so the lease
            // can say which client bound where. Roots-mode leases register
            // after this handshake and carry it immediately; Fixed-mode
            // leases registered before it pick it up on the next renewal.
            let name = init.params.client_info.name.trim().to_string();
            if !name.is_empty() {
                *self.state.client_name.lock().unwrap() = Some(name);
            }
            return Ok(rmcp::model::ServerResult::InitializeResult(
                self.info.clone(),
            ));
        }
        // Every other early request (a tools/list right after initialize)
        // holds until the session is bound to a project, so nothing lands in
        // the wrong graph while roots are still being resolved.
        let peer = self.state.peer_when_bound().await?;
        peer.send_request(request).await.map_err(proxy_err)
    }

    async fn handle_notification(
        &self,
        notification: rmcp::model::ClientNotification,
        context: rmcp::service::NotificationContext<rmcp::RoleServer>,
    ) -> Result<(), ErrorData> {
        if let Some(roots) = &self.roots {
            match &notification {
                rmcp::model::ClientNotification::InitializedNotification(_) => {
                    // The binding trigger: roots/list is only spec-legal
                    // after this. Not replayed upstream — the upstream
                    // session runs its own handshake.
                    let _ = roots.initialized.send(true);
                    return Ok(());
                }
                rmcp::model::ClientNotification::RootsListChangedNotification(_) => {
                    // Re-ask and rebind off-thread; the notification itself
                    // has no reply to hold up.
                    let binding = roots.clone();
                    let peer = context.peer.clone();
                    tokio::spawn(async move { binding.rebind_from(&peer).await });
                    return Ok(());
                }
                _ => {}
            }
        }
        let peer = self.state.peer_when_bound().await?;
        peer.send_notification(notification)
            .await
            .map_err(proxy_err)
    }

    fn get_info(&self) -> rmcp::model::ServerInfo {
        self.info.clone()
    }
}

/// How `serve_stdio_bridge` finds its upstream.
pub enum BridgeTarget {
    /// Explicit `--db`: one fixed upstream, resolved AFTER the stdio
    /// handshake is answered (handshake-first, the 60s-client rule: a slow
    /// or failed core boot must starve the bind step, never initialize).
    /// Roots are ignored — an explicit db is an explicit answer — and the
    /// proxy forwards verbatim once connected.
    Fixed {
        /// Maps the fixed db to its upstream MCP target — spawning the
        /// machine core on the way when none runs. Blocking is fine — the
        /// bridge calls it through `spawn_blocking`, after serving.
        resolve: FixedResolver,
    },
    /// No `--db`: bind by the client's MCP roots, falling back to
    /// `fallback_root` (the bridge's cwd) when the client doesn't advertise
    /// the capability, answers with no `file://` root, or doesn't answer
    /// within the roots timeout.
    Roots {
        fallback_root: std::path::PathBuf,
        resolve: RootResolver,
    },
}

/// Where a resolved root's session should connect, and which root the census
/// lease should announce. `lease_root` is normally the project root itself;
/// a resolver that decided the root can't host a project (unwritable IDE
/// launch cwd) points `url` at the core's home-graph endpoint and
/// `lease_root` at the engram home dir instead.
pub struct ResolvedTarget {
    pub url: String,
    pub lease_root: String,
}

/// Maps a project root to the upstream MCP target serving it (registering
/// the project with the machine core on the way). Blocking is fine — the
/// bridge calls it through `spawn_blocking`.
pub type RootResolver =
    Arc<dyn Fn(std::path::PathBuf) -> anyhow::Result<ResolvedTarget> + Send + Sync>;

/// Maps an explicit `--db` to the upstream MCP target serving it (spawning
/// the machine core on the way when none runs). Blocking is fine — the
/// bridge calls it through `spawn_blocking`, after the stdio transport is
/// already answering the handshake.
pub type FixedResolver = Arc<dyn Fn() -> anyhow::Result<ResolvedTarget> + Send + Sync>;

/// One live upstream MCP session. Generations order rebinds: a watcher or
/// heartbeat that saw generation N stays quiet when the current one moved on.
struct Upstream {
    peer: rmcp::service::Peer<rmcp::RoleClient>,
    generation: u64,
    cancel: Option<rmcp::service::RunningServiceCancellationToken>,
}

/// The bridge's census identity: where the core's lease API lives and which
/// project root this bridge currently serves. Registration is best-effort
/// observability — an older core without `/clients` (or a failed POST) never
/// blocks bridging.
#[derive(Clone)]
struct LeaseState {
    /// The core's origin, e.g. `http://127.0.0.1:8787`.
    base_url: String,
    /// Absolute project root the bridge is bound to.
    root: String,
    lease_id: Option<String>,
}

/// Everything the proxy, the heartbeat, and the binder share.
struct BridgeState {
    http: reqwest::Client,
    slot: std::sync::Mutex<Option<Upstream>>,
    /// Latest bound generation (0 = not bound yet); `peer_when_bound` waits
    /// on it so early requests can't race the first binding.
    bound: tokio::sync::watch::Sender<u64>,
    lease: std::sync::Mutex<Option<LeaseState>>,
    /// `clientInfo.name` from the stdio client's initialize, once seen —
    /// carried on census registrations and renewals so the core can say
    /// which MCP client (Claude Code, mcp-go/Windsurf, …) holds each lease.
    client_name: std::sync::Mutex<Option<String>>,
    /// Fatal bridge errors (core closed the current session, binding failed)
    /// funnel here; the main select exits on the first one.
    exit: tokio::sync::mpsc::Sender<anyhow::Error>,
    /// Why the binding failed for good, when it did. `peer_when_bound` stops
    /// waiting and answers held client requests with this instead of letting
    /// them die unanswered when the bridge exits (the Windsurf field trace:
    /// a silent close reads as "Failed to initialize server" with zero
    /// forensics on the client side).
    failed: std::sync::Mutex<Option<String>>,
}

impl BridgeState {
    fn current_peer(&self) -> Option<(rmcp::service::Peer<rmcp::RoleClient>, u64)> {
        self.slot
            .lock()
            .unwrap()
            .as_ref()
            .map(|u| (u.peer.clone(), u.generation))
    }

    fn current_generation(&self) -> u64 {
        self.slot
            .lock()
            .unwrap()
            .as_ref()
            .map(|u| u.generation)
            .unwrap_or(0)
    }

    /// The current upstream peer, waiting out an in-flight (re)binding.
    /// Binding is bounded — initialized grace + roots timeout + local HTTP,
    /// plus (handshake-first) the ensure-core work: a first-run provision
    /// can hold the spawned core's 180s health wait — so the cap is
    /// generous slack over the worst legitimate bind, not an expected
    /// wait. A binding that FAILS never waits it out: [`fail`] wakes every
    /// held request immediately with the real error.
    async fn peer_when_bound(&self) -> Result<rmcp::service::Peer<rmcp::RoleClient>, ErrorData> {
        if let Some((peer, _)) = self.current_peer() {
            return Ok(peer);
        }
        let mut rx = self.bound.subscribe();
        let bound = async {
            loop {
                if *rx.borrow_and_update() > 0 {
                    return;
                }
                if rx.changed().await.is_err() {
                    return;
                }
            }
        };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(210), bound).await;
        self.current_peer().map(|(peer, _)| peer).ok_or_else(|| {
            let why = self.failed.lock().unwrap().clone().unwrap_or_else(|| {
                "the engram bridge has no project binding yet — the core may be unreachable \
                 (see .engram/mcp.log, or ~/.engram/mcp.log when the launch cwd is unwritable)"
                    .into()
            });
            ErrorData::internal_error(why, None)
        })
    }

    /// Record a fatal binding failure and wake everything waiting in
    /// [`peer_when_bound`] — held requests answer with a JSON-RPC error
    /// carrying `why` instead of dying unanswered when the bridge exits.
    fn fail(&self, why: String) {
        *self.failed.lock().unwrap() = Some(why);
        // u64::MAX satisfies the "bound moved" wake without ever being a
        // real generation; the woken waiter finds no peer and reads
        // `failed` for the message.
        let _ = self.bound.send(u64::MAX);
    }

    /// Open a new upstream session to `url` and make it current. The old
    /// session (a rebind) is cancelled — its transport DELETEs the HTTP
    /// session server-side on teardown, so no zombie session lingers with a
    /// stale project binding — and the census lease is re-pointed (renewed
    /// in place, never duplicated: the census keys on lease_id).
    async fn connect(
        self: &Arc<Self>,
        url: &str,
        lease_root: Option<&str>,
    ) -> anyhow::Result<rmcp::model::ServerInfo> {
        let transport = rmcp::transport::StreamableHttpClientTransport::with_client(
            self.http.clone(),
            rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(
                url.to_string(),
            ),
        );
        let client = ().serve(transport).await?;
        let info = client
            .peer()
            .peer_info()
            .map(|i| (*i).clone())
            .ok_or_else(|| anyhow::anyhow!("daemon MCP handshake returned no server info"))?;
        let peer = client.peer().clone();
        let cancel = client.cancellation_token();
        let (old, generation) = {
            let mut slot = self.slot.lock().unwrap();
            let generation = slot.as_ref().map(|u| u.generation).unwrap_or(0) + 1;
            let old = slot.replace(Upstream {
                peer,
                generation,
                cancel: Some(cancel),
            });
            (old, generation)
        };
        // The upstream ending while still current means the core closed the
        // session (orchestrated shutdown) — exit right away instead of
        // waiting out a heartbeat. A cancelled predecessor lands here too,
        // sees its generation is history, and stays quiet.
        let state = self.clone();
        tokio::spawn(async move {
            let _ = client.waiting().await;
            if state.current_generation() == generation {
                let _ = state
                    .exit
                    .send(anyhow::anyhow!(
                        "the engram core closed the session — bridge exiting"
                    ))
                    .await;
            }
        });
        if let Some(mut old) = old
            && let Some(token) = old.cancel.take()
        {
            token.cancel();
        }
        if let Some(root) = lease_root
            && let Some(base_url) = origin_of(url)
        {
            self.update_lease(base_url, root.to_string()).await;
        }
        let _ = self.bound.send(generation);
        Ok(info)
    }

    /// Point the census lease at (base_url, root). An existing lease on the
    /// same core is renewed with the new root — same lease_id, the census
    /// row just moves; a lease on a different core (repo-local daemon vs
    /// machine core) is withdrawn and re-registered there.
    async fn update_lease(&self, base_url: String, root: String) {
        let prev = self.lease.lock().unwrap().clone();
        let name = self.client_name.lock().unwrap().clone();
        let name = name.as_deref();
        let lease_id = match prev {
            Some(prev) if prev.base_url == base_url => match prev.lease_id {
                Some(id) if ping_lease(&self.http, &base_url, &id, Some(&root), name).await => {
                    Some(id)
                }
                _ => register_lease(&self.http, &base_url, &root, name).await,
            },
            Some(prev) => {
                if let Some(id) = prev.lease_id {
                    delete_lease(&self.http, &prev.base_url, &id).await;
                }
                register_lease(&self.http, &base_url, &root, name).await
            }
            None => register_lease(&self.http, &base_url, &root, name).await,
        };
        *self.lease.lock().unwrap() = Some(LeaseState {
            base_url,
            root,
            lease_id,
        });
    }

    /// Satellites die with the core (v0.6.2). The HTTP client transport
    /// auto-reconnects, so a dead core never surfaces as a closed connection
    /// — liveness has to be probed: ping the core on a heartbeat and treat a
    /// timed-out or failed ping as its death, unless a rebind swapped the
    /// session out from under the ping. Census lease renewals ride the same
    /// beat. Returns the fatal error when the core is gone.
    async fn heartbeat(self: Arc<Self>) -> anyhow::Error {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(heartbeat_secs())).await;
            let Some((peer, generation)) = self.current_peer() else {
                // Not bound yet — the initial binding is still in flight.
                continue;
            };
            let ping = peer.send_request(rmcp::model::ClientRequest::PingRequest(
                rmcp::model::PingRequest {
                    method: Default::default(),
                    extensions: Default::default(),
                },
            ));
            match tokio::time::timeout(std::time::Duration::from_secs(10), ping).await {
                Ok(Ok(_)) => {}
                _ => {
                    if self.current_generation() == generation {
                        return anyhow::anyhow!("the engram core went away — bridge exiting");
                    }
                    continue; // a rebind cut this ping mid-flight — not a death
                }
            }
            // Renew the census lease. Best-effort: a 404 means the lease
            // expired (or the core restarted its table) — re-register
            // instead of silently vanishing from the census.
            let lease = self.lease.lock().unwrap().clone();
            if let Some(l) = lease {
                let name = self.client_name.lock().unwrap().clone();
                let renewed = match &l.lease_id {
                    Some(id) => {
                        ping_lease(&self.http, &l.base_url, id, Some(&l.root), name.as_deref())
                            .await
                    }
                    None => false,
                };
                if !renewed {
                    let id =
                        register_lease(&self.http, &l.base_url, &l.root, name.as_deref()).await;
                    let mut slot = self.lease.lock().unwrap();
                    match slot.as_mut() {
                        // Only fill the row we renewed for — a rebind that
                        // raced this re-register owns the slot now.
                        Some(cur) if cur.base_url == l.base_url && cur.root == l.root => {
                            cur.lease_id = id;
                        }
                        _ => {
                            if let Some(id) = id {
                                let http = self.http.clone();
                                let base = l.base_url.clone();
                                tokio::spawn(async move {
                                    delete_lease(&http, &base, &id).await;
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The roots half of the bridge's one bounded brain (0.8.8): which project
/// root the session is bound to, and how to move it.
struct RootsBinding {
    fallback_root: std::path::PathBuf,
    resolve: RootResolver,
    state: Arc<BridgeState>,
    /// Canonicalized root of the current binding.
    current_root: std::sync::Mutex<Option<std::path::PathBuf>>,
    /// Serializes bind/rebind so two triggers can't interleave connects.
    bind_gate: tokio::sync::Mutex<()>,
    /// Flipped by the client's `notifications/initialized` — the earliest
    /// spec-legal moment to send our roots/list request.
    initialized: tokio::sync::watch::Sender<bool>,
}

/// How long the bridge waits for a roots/list answer before falling back to
/// cwd — a client that advertises roots but never answers must not hang the
/// session.
const ROOTS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl RootsBinding {
    /// Ask the client for its roots; the first `file://` root wins. None on
    /// timeout, error, or an empty/non-file answer — the caller falls back.
    async fn query_roots(
        &self,
        peer: &rmcp::service::Peer<rmcp::RoleServer>,
    ) -> Option<std::path::PathBuf> {
        #[allow(deprecated)] // SEP-2577 deprecates roots; clients still speak it
        match tokio::time::timeout(ROOTS_TIMEOUT, peer.list_roots()).await {
            Ok(Ok(listed)) => {
                let root = listed.roots.iter().find_map(|r| file_uri_to_path(&r.uri));
                if root.is_none() {
                    tracing::info!("client roots carry no file:// root — falling back to cwd");
                }
                root
            }
            Ok(Err(e)) => {
                tracing::warn!("roots/list failed ({e}) — falling back to cwd");
                None
            }
            Err(_) => {
                tracing::warn!(
                    "roots/list unanswered after {ROOTS_TIMEOUT:?} — falling back to cwd"
                );
                None
            }
        }
    }

    /// Bind the session to `root`: resolve its upstream URL (registering the
    /// project with the core), open the new session, retire the old one.
    /// No-op when already bound there.
    async fn bind_to(&self, root: std::path::PathBuf) -> anyhow::Result<()> {
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let _gate = self.bind_gate.lock().await;
        if self.current_root.lock().unwrap().as_deref() == Some(&root) {
            return Ok(());
        }
        let resolve = self.resolve.clone();
        let resolving = root.clone();
        let target = tokio::task::spawn_blocking(move || resolve(resolving)).await??;
        self.state
            .connect(&target.url, Some(&target.lease_root))
            .await?;
        tracing::info!("bridge bound to {} ({})", target.lease_root, target.url);
        *self.current_root.lock().unwrap() = Some(root);
        Ok(())
    }

    /// The initial binding, run once the stdio handshake is done: roots when
    /// the client advertises them (after a short grace for its initialized
    /// notification), cwd otherwise.
    async fn initial_bind(
        self: Arc<Self>,
        peer: rmcp::service::Peer<rmcp::RoleServer>,
        client_has_roots: bool,
    ) -> anyhow::Result<()> {
        let root = if client_has_roots {
            let mut rx = self.initialized.subscribe();
            let initialized = async {
                loop {
                    if *rx.borrow_and_update() {
                        return;
                    }
                    if rx.changed().await.is_err() {
                        return;
                    }
                }
            };
            // A client that skips initialized still gets asked after the
            // grace — the roots timeout bounds the total either way.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), initialized).await;
            match self.query_roots(&peer).await {
                Some(root) => {
                    tracing::info!("binding by client roots: {}", root.display());
                    root
                }
                None => {
                    tracing::info!("binding by cwd fallback: {}", self.fallback_root.display());
                    self.fallback_root.clone()
                }
            }
        } else {
            tracing::info!(
                "client advertises no roots — binding by cwd fallback: {}",
                self.fallback_root.display()
            );
            self.fallback_root.clone()
        };
        self.bind_to(root).await
    }

    /// roots/list_changed: re-ask, rebind when the project changed. A failed
    /// rebind keeps the current binding — better a stale project than a dead
    /// session — and logs why.
    async fn rebind_from(self: Arc<Self>, peer: &rmcp::service::Peer<rmcp::RoleServer>) {
        let root = self
            .query_roots(peer)
            .await
            .unwrap_or_else(|| self.fallback_root.clone());
        if let Err(e) = self.bind_to(root).await {
            tracing::warn!(
                "rebind after roots/list_changed failed — keeping the current project: {e:#}"
            );
        }
    }
}

/// `file:///Users/x/repo` → `/Users/x/repo`. Tolerates an authority
/// (`file://localhost/…`) and percent-encoding; anything else is None.
fn file_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(i) if &rest[..i] == "localhost" => &rest[i..],
        _ => return None,
    };
    let decoded = percent_decode(path);
    #[cfg(windows)]
    let decoded = {
        // file:///C:/dir arrives as /C:/dir — drop the leading slash.
        let bytes = decoded.as_bytes();
        if bytes.len() >= 3
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b':'
        {
            decoded[1..].to_string()
        } else {
            decoded
        }
    };
    Some(std::path::PathBuf::from(decoded))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `http://127.0.0.1:8787/projects/x/mcp` → `http://127.0.0.1:8787`.
fn origin_of(url: &str) -> Option<String> {
    let after_scheme = url.find("://")? + 3;
    let end = url[after_scheme..]
        .find('/')
        .map(|i| after_scheme + i)
        .unwrap_or(url.len());
    Some(url[..end].to_string())
}

/// The bridge's heartbeat cadence — core-liveness pings AND lease renewals
/// ride it. 15s in production (the core's 45s lease TTL is three beats);
/// `ENGRAM_BRIDGE_HEARTBEAT_SECS` shortens it for sandboxed tests, which
/// also shorten the TTL and would otherwise see live leases flap.
fn heartbeat_secs() -> u64 {
    std::env::var("ENGRAM_BRIDGE_HEARTBEAT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(15)
}

async fn register_lease(
    client: &reqwest::Client,
    base_url: &str,
    root: &str,
    client_name: Option<&str>,
) -> Option<String> {
    let resp = client
        .post(format!("{base_url}/clients"))
        .json(&serde_json::json!({
            "pid": std::process::id(),
            "kind": "mcp-bridge",
            "root": root,
            "client": client_name,
        }))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let body: serde_json::Value = resp.json().await.ok()?;
    body["lease_id"].as_str().map(str::to_string)
}

/// Renew a lease, carrying the (possibly rebound) root — and the client name
/// once known — so the census row follows the binding. Older cores ignore
/// the body — the renewal still counts. False = the lease is gone (expired,
/// core restarted).
async fn ping_lease(
    client: &reqwest::Client,
    base_url: &str,
    id: &str,
    root: Option<&str>,
    client_name: Option<&str>,
) -> bool {
    let mut req = client
        .post(format!("{base_url}/clients/{id}/ping"))
        .timeout(std::time::Duration::from_secs(5));
    if root.is_some() || client_name.is_some() {
        req = req.json(&serde_json::json!({ "root": root, "client": client_name }));
    }
    req.send().await.is_ok_and(|r| r.status().is_success())
}

async fn delete_lease(client: &reqwest::Client, base_url: &str, id: &str) {
    let _ = client
        .delete(format!("{base_url}/clients/{id}"))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await;
}

/// Serve stdio by bridging every message to a core's MCP endpoint until the
/// stdio client disconnects (or the core goes away — satellites die with the
/// core, v0.6.2). Both shapes serve the handshake FIRST and connect after —
/// a core that boots slowly (first-run model provisioning) or not at all
/// must never leave initialize unanswered (impatient clients kill a silent
/// server at ~60s). `Fixed` resolves its one upstream in the background and
/// forwards verbatim; `Roots` binds from the client's roots (cwd fallback)
/// and rebinds on roots/list_changed. Either way, a failed resolution goes
/// through [`BridgeState::fail`] so held requests answer with the real
/// error before the bridge exits.
pub async fn serve_stdio_bridge(target: BridgeTarget) -> anyhow::Result<()> {
    // The core is always on 127.0.0.1, but reqwest honors HTTP(S)_PROXY env
    // vars by default — under a corporate proxy the loopback connection gets
    // routed through it and dies with the proxy's HTML error page (issue #2).
    // No proxy ever makes sense here, so opt out instead of asking users to
    // set NO_PROXY.
    let http = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|e| anyhow::anyhow!("building the bridge HTTP client: {e}"))?;
    let (bound, _bound_rx) = tokio::sync::watch::channel(0u64);
    let (exit, mut exit_rx) = tokio::sync::mpsc::channel::<anyhow::Error>(4);
    let state = Arc::new(BridgeState {
        http,
        slot: std::sync::Mutex::new(None),
        bound,
        lease: std::sync::Mutex::new(None),
        client_name: std::sync::Mutex::new(None),
        exit,
        failed: std::sync::Mutex::new(None),
    });
    // In both shapes the upstream doesn't exist yet when the stdio client's
    // initialize arrives (handshake-first), so it is answered from this
    // crate's own contract — identical to what any 0.8.x core negotiates,
    // because both sides construct it here.
    let (roots, fixed) = match target {
        BridgeTarget::Fixed { resolve } => (None, Some(resolve)),
        BridgeTarget::Roots {
            fallback_root,
            resolve,
        } => {
            let (initialized, _) = tokio::sync::watch::channel(false);
            let binding = Arc::new(RootsBinding {
                fallback_root,
                resolve,
                state: state.clone(),
                current_root: std::sync::Mutex::new(None),
                bind_gate: tokio::sync::Mutex::new(()),
                initialized,
            });
            (Some(binding), None)
        }
    };
    let info = engram_server_info();
    let proxy = Passthrough {
        state: state.clone(),
        info,
        roots: roots.clone(),
    };
    let service = proxy.serve(rmcp::transport::io::stdio()).await?;
    if let Some(resolve) = fixed {
        // The Fixed connect runs AFTER the transport is serving, so the
        // handshake answers while the core comes up (a first-run provision
        // can take minutes); early tool calls hold in `peer_when_bound`.
        let fatal = state.exit.clone();
        let fail_state = state.clone();
        tokio::spawn(async move {
            let resolved = tokio::task::spawn_blocking(move || resolve())
                .await
                .map_err(anyhow::Error::from)
                .and_then(|r| r);
            let connected = match resolved {
                Ok(t) => fail_state
                    .connect(&t.url, Some(&t.lease_root))
                    .await
                    .map(|_| ()),
                Err(e) => Err(e),
            };
            if let Err(e) = connected {
                let e = e.context("connecting the bridge to its core");
                // Answer every held client request with the real reason
                // before exiting — a silent close is "Failed to initialize
                // server" with zero forensics (the Windsurf field trace).
                fail_state.fail(format!("engram bridge: {e:#}"));
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let _ = fatal.send(e).await;
            }
        });
    }
    if let Some(binding) = roots {
        let peer = service.peer().clone();
        #[allow(deprecated)] // SEP-2577 deprecates roots; clients still speak it
        let client_has_roots = peer
            .peer_info()
            .is_some_and(|i| i.capabilities.roots.is_some());
        let fatal = state.exit.clone();
        let fail_state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = binding.initial_bind(peer, client_has_roots).await {
                let e = e.context("binding the bridge to a project");
                // Answer every held client request with the real reason
                // before exiting — a silent close is "Failed to initialize
                // server" with zero forensics (the Windsurf field trace).
                fail_state.fail(format!("engram bridge: {e:#}"));
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let _ = fatal.send(e).await;
            }
        });
    }
    // Whichever side ends first ends the bridge — stdio closing is a normal
    // disconnect; the upstream dying (watcher) or going silent (heartbeat)
    // means the core is gone and lingering would only strand the client.
    let result = tokio::select! {
        served = service.waiting() => served.map(|_| ()).map_err(anyhow::Error::from),
        Some(err) = exit_rx.recv() => Err(err),
        err = state.clone().heartbeat() => Err(err),
    };
    // Clean exit withdraws the lease instead of letting it lapse.
    let parting = state.lease.lock().unwrap().take();
    if let Some(l) = parting
        && let Some(id) = l.lease_id
    {
        delete_lease(&state.http, &l.base_url, &id).await;
    }
    result
}

// ---- argument schemas ---------------------------------------------------
//
// Every scoped tool takes the same optional `project` selector (PLAN §7C):
// omitted = the current project; a registered name, id, or project DIRECTORY
// (any path inside the repo — what a hook or a bridge holds) = that project
// (reads AND writes — capturing into a sibling repo's graph is deliberate,
// and the longest matching root wins for nested projects); "home" =
// the user-level home graph; "all" = every project, reads only (search /
// check_claim). `list_projects` names what exists.

#[derive(Deserialize, JsonSchema, Default)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    types: Vec<String>,
    #[serde(default)]
    limit: Option<usize>,
    /// Progressive disclosure: "compact" = id/type/title/score/trust only
    /// (cheapest — start here when scanning), "snippet" (default) = compact
    /// plus matched snippet and 1-hop neighbors, "full" = snippet plus the
    /// complete body of every hit. Expand on demand instead of paying for
    /// depth up front.
    #[serde(default)]
    detail: Option<String>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home"; "all" = every project + home with provenance
    /// (reads only).
    #[serde(default)]
    project: Option<String>,
    /// "auto" (default) = curated memory, with session history appearing as
    /// a separate labeled section only when the confidence verdict says the
    /// answer is likely not in memory; "memory" = curated only;
    /// "history" = session history only.
    #[serde(default)]
    scope: Option<String>,
    /// Only knowledge captured at or after this instant. Takes a day
    /// ("2026-08-14"), an ISO instant ("2026-08-14T09:15:32Z"), or a relative
    /// expression the daemon resolves ("today", "yesterday", "last week",
    /// "last 3 days", "2 hours ago", "a month ago", "this year"). Relative
    /// expressions mean the START of the span, so after: "last week" reads
    /// "since a week ago".
    #[serde(default)]
    after: Option<String>,
    /// Only knowledge captured strictly before this instant — same grammar as
    /// `after`. Pair them for a bounded window.
    #[serde(default)]
    before: Option<String>,
    /// Only knowledge captured while this working version was current (e.g.
    /// "0.8.4"), resolved from the graph's recorded `set_version` switches.
    /// Combines with after/before, which narrow it further.
    #[serde(default)]
    during_version: Option<String>,
    /// "relevance" (default) = score order, the ranking the confidence verdict
    /// is defined against; "chronological" = oldest first, for reading how
    /// something developed; "recent" = newest first, for "what is the CURRENT
    /// value of X". Ordering is applied after every cut, so it re-orders the
    /// delivered set without changing which hits were delivered.
    #[serde(default)]
    order: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ExpandHistoryArgs {
    /// The session handle a history hit carried.
    session: String,
    /// The turn to center on.
    turn: u64,
    /// Messages of context on each side (default 4, max 25).
    #[serde(default)]
    window: Option<u64>,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema, Default)]
struct ListSessionsArgs {
    /// Max sessions returned, newest first (default 20, max 100).
    #[serde(default)]
    limit: Option<usize>,
    /// Only sessions of one harness ("claude-code", "codex", "bob", …).
    #[serde(default)]
    harness: Option<String>,
    /// Only sessions overlapping the window starting here — same grammar as
    /// search's `after` (a day, an ISO instant, or "last week" / "yesterday" /
    /// "3 days ago"). A session that began earlier and ran into the window
    /// counts as inside it.
    #[serde(default)]
    after: Option<String>,
    /// Only sessions that had started before this instant — same grammar.
    #[serde(default)]
    before: Option<String>,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct IdArg {
    id: String,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ProjectArg {
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct AuditArgs {
    /// Max rows to return (default 20, newest first).
    #[serde(default)]
    limit: Option<usize>,
    /// Keyset cursor: only rows with seq strictly below this (page with the
    /// last row's seq).
    #[serde(default)]
    before: Option<i64>,
    /// Restrict to one node/edge id's history.
    #[serde(default)]
    entity_id: Option<String>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct GetNodeArgs {
    id: String,
    /// Levels of parent hierarchy to include (nodes this one points at), 0-3.
    #[serde(default)]
    parents: Option<usize>,
    /// Levels of child hierarchy to include (nodes pointing at this one), 0-3.
    #[serde(default)]
    children: Option<usize>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct TraverseArgs {
    from: String,
    #[serde(default)]
    edge_types: Vec<String>,
    #[serde(default)]
    depth: Option<usize>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct AddNoteArgs {
    #[serde(rename = "type")]
    node_type: String,
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    durability: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    /// Captured-at version override (version tracking): omit to auto-stamp
    /// the graph's current working version on version-bound types.
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    code_refs: Vec<String>,
    /// Free-form slice labels (kebab-cased on write). Reuse the recent tags
    /// listed in the brief before inventing new ones; new tags are created
    /// implicitly.
    #[serde(default)]
    tags: Vec<String>,
    /// The knowledge's ORIGINAL date, for digesting historical material:
    /// "YYYY-MM-DD" (or unix seconds). Omit for live capture — then the note
    /// is dated now. Never use for fresh knowledge.
    #[serde(default)]
    created_at: Option<String>,
    /// Omit = current project; a name, id, or directory writes into THAT
    /// project's graph (deliberate cross-project capture); "home" = the
    /// user-level graph.
    /// "all" is refused — a fanned-out write is replication.
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct LinkArgs {
    from: String,
    to: String,
    #[serde(rename = "type")]
    edge_type: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project (both endpoints must live there — edges never cross
    /// graphs); "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct UpdateArgs {
    id: String,
    /// Reclassify the node (one of the 8 canonical types).
    #[serde(default, rename = "type")]
    node_type: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    durability: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    code_refs: Option<Vec<String>>,
    /// Replaces the node's tag list when set (kebab-cased on write).
    #[serde(default)]
    tags: Option<Vec<String>>,
    /// Set or correct the node's captured-at version (version tracking).
    #[serde(default)]
    version: Option<String>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct MergeArgs {
    /// The node that lives on and receives the union.
    survivor: String,
    /// Nodes to fold into the survivor — each ends archived behind a
    /// replaces edge, its live edges moved to the survivor.
    victims: Vec<String>,
    /// Merged title, composed from the parts (omit = survivor's stands).
    #[serde(default)]
    title: Option<String>,
    /// Merged body, composed from the parts (omit = survivor's stands —
    /// the victims' bodies are NOT appended automatically).
    #[serde(default)]
    body: Option<String>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct AddNotesArgs {
    /// Notes to create; each item takes the same fields as add_note.
    notes: Vec<AddNoteArgs>,
}

#[derive(Deserialize, JsonSchema)]
struct UpdateNodesArgs {
    /// Patches to apply; each item takes the same fields as update_node.
    updates: Vec<UpdateArgs>,
}

#[derive(Deserialize, JsonSchema)]
struct ListNodesArgs {
    /// Filter to these node types (default: all 8).
    #[serde(default)]
    types: Vec<String>,
    /// Filter Problems/Intents by status: open | resolved | obsolete.
    #[serde(default)]
    status: Option<String>,
    /// Only nodes carrying this tag.
    #[serde(default)]
    tag: Option<String>,
    /// Ordering: "recent" (default, newest first), "most-connected" or
    /// "least-connected" (by live edge count — the least-connected end is
    /// where unreachable knowledge hides).
    #[serde(default)]
    sort: Option<String>,
    /// Also return archived (superseded) generations. Default false.
    #[serde(default)]
    include_archived: Option<bool>,
    /// true = only user-pinned (constant-trust) nodes; false = only unpinned.
    #[serde(default)]
    pinned: Option<bool>,
    /// Page size (default 30, max 200).
    #[serde(default)]
    limit: Option<usize>,
    /// Skip this many (after filtering, newest first).
    #[serde(default)]
    offset: Option<usize>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ApproveArgs {
    id: String,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct CheckClaimArgs {
    /// The claim to verify, as one declarative sentence.
    claim: String,
    /// How many nearby nodes to judge (default 8, max 16).
    #[serde(default)]
    limit: Option<usize>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home"; "all" = judge across every project + home with
    /// provenance.
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ResolveSuspectArgs {
    /// The suspect id (from the brief's "Suspected conflicts" section or list_suspects).
    id: String,
    /// "conflict" | "replaces" | "dismiss"
    verdict: String,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ListOpenArgs {
    #[serde(default)]
    types: Vec<String>,
    #[serde(default)]
    include_conflicts: Option<bool>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct BriefArgs {
    /// Character budget for the digest (default ~16000, about 4k tokens).
    #[serde(default)]
    max_chars: Option<usize>,
    /// Omit = this session's project plus the home-graph section; a name,
    /// an id, a project directory (or "home") briefs that graph alone.
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct SetVersionArgs {
    /// The new current version ("v0.7.0", "26.7.23", …); null/omit clears it.
    #[serde(default)]
    version: Option<String>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct DescribeOntologyArgs {
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct UpdateEdgeArgs {
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    project: Option<String>,
}

// ---- helpers ------------------------------------------------------------

/// The store marks matches with private-use sentinels (the pane's highlight
/// markers); assistants read plain brackets instead.
fn debracket(hit: &mut engram_core::SearchHit) {
    hit.snippet = hit
        .snippet
        .replace(engram_core::SNIPPET_OPEN, "[")
        .replace(engram_core::SNIPPET_CLOSE, "]");
}

fn ok_json<T: Serialize>(v: &T) -> Result<CallToolResult, ErrorData> {
    let text = serde_json::to_string_pretty(v)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

const HIERARCHY_MAX_DEPTH: usize = 3;
const HIERARCHY_MAX_BREADTH: usize = 8;

/// Recursive reasoning hierarchy around a node. `up` follows outgoing edges
/// (parents: what this node stands on / is about); `!up` follows incoming
/// (children: what answers, builds on, or contradicts it). Depth and breadth
/// are capped and cycles cut so the payload stays context-window friendly.
fn hierarchy(
    engine: &engram_core::Engine,
    id: &str,
    depth: usize,
    up: bool,
    seen: &mut std::collections::HashSet<String>,
) -> Vec<serde_json::Value> {
    if depth == 0 {
        return Vec::new();
    }
    let edges = if up {
        engine.edges_out(id)
    } else {
        engine.edges_in(id)
    }
    .unwrap_or_default();
    let mut out = Vec::new();
    for e in edges.into_iter().take(HIERARCHY_MAX_BREADTH) {
        let other = if up { &e.to_id } else { &e.from_id };
        if !seen.insert(other.clone()) {
            continue;
        }
        let Ok(Some(n)) = engine.get_node(other) else {
            continue;
        };
        let deeper = hierarchy(engine, other, depth - 1, up, seen);
        let mut item = json!({
            "edge": e.edge_type.as_str(),
            "node": {
                "id": n.id,
                "type": n.node_type.as_str(),
                "title": n.title,
                "status": n.status.map(|s| s.as_str()),
                "trust": (n.trust * 100.0).round() / 100.0,
                "stale": n.stale,
                "archived": n.valid_until.is_some(),
            }
        });
        if !deeper.is_empty() {
            item[if up { "parents" } else { "children" }] = json!(deeper);
        }
        out.push(item);
    }
    out
}

fn map_err(e: Error) -> ErrorData {
    match e {
        Error::NotFound(s) => ErrorData::invalid_params(format!("not found: {s}"), None),
        e @ (Error::Parse { .. } | Error::Pinned(_) | Error::Project(_)) => {
            ErrorData::invalid_params(e.to_string(), None)
        }
        e => ErrorData::internal_error(e.to_string(), None),
    }
}

fn node_types(v: &[String]) -> Result<Vec<NodeType>, ErrorData> {
    v.iter()
        .map(|s| NodeType::parse(s))
        .collect::<engram_core::Result<_>>()
        .map_err(map_err)
}

fn edge_types(v: &[String]) -> Result<Vec<EdgeType>, ErrorData> {
    v.iter()
        .map(|s| EdgeType::parse(s))
        .collect::<engram_core::Result<_>>()
        .map_err(map_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_resolve_to_local_paths_or_nothing() {
        let p = |s: &str| Some(std::path::PathBuf::from(s));
        assert_eq!(file_uri_to_path("file:///Users/x/repo"), p("/Users/x/repo"));
        assert_eq!(file_uri_to_path("file://localhost/x"), p("/x"));
        assert_eq!(file_uri_to_path("file:///a%20b/c"), p("/a b/c"));
        assert_eq!(file_uri_to_path("file://remote-host/x"), None);
        assert_eq!(file_uri_to_path("https://example.com/x"), None);
        assert_eq!(file_uri_to_path("file://"), None);
    }

    #[test]
    fn shipped_ontology_durability_defaults_hold() {
        let cfg = engram_core::config::GraphConfig::default();
        let durability = |name: &str| cfg.type_def(name).unwrap().durability;
        assert_eq!(durability("Decision"), Durability::Stable);
        assert_eq!(durability("Insight"), Durability::Episodic);
        assert_eq!(durability("Intent"), Durability::Volatile);
    }

    #[test]
    fn type_parsing_is_shape_only() {
        // Ontology-as-data (PLAN §7D): names parse by shape; whether one
        // exists is the engine's config-driven check, so a custom ontology's
        // types flow through the MCP layer untouched.
        assert!(node_types(&["Decision".into()]).is_ok());
        assert!(node_types(&["Nope".into()]).is_ok());
        assert!(node_types(&["".into()]).is_err());
        assert!(edge_types(&["because".into()]).is_ok());
        assert!(edge_types(&["relates_to".into()]).is_ok());
        assert!(edge_types(&["".into()]).is_err());
    }

    /// The silent-death fix (Windsurf field trace): a fatal binding failure
    /// must wake every request held in `peer_when_bound` with the real
    /// reason, immediately — not let them ride out the 30s cap and die
    /// unanswered when the bridge exits.
    #[tokio::test]
    async fn failed_binding_answers_held_and_future_requests() {
        let (bound, _bound_rx) = tokio::sync::watch::channel(0u64);
        let (exit, _exit_rx) = tokio::sync::mpsc::channel(4);
        let state = Arc::new(BridgeState {
            http: reqwest::Client::new(),
            slot: std::sync::Mutex::new(None),
            bound,
            lease: std::sync::Mutex::new(None),
            client_name: std::sync::Mutex::new(None),
            exit,
            failed: std::sync::Mutex::new(None),
        });
        // A request already waiting on the binding…
        let held = {
            let state = state.clone();
            tokio::spawn(async move { state.peer_when_bound().await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        state.fail("engram bridge: no core is reachable".into());
        // …answers with the failure right away (well under the 30s cap).
        let err = tokio::time::timeout(std::time::Duration::from_secs(2), held)
            .await
            .expect("the held request must wake on failure")
            .unwrap()
            .expect_err("no peer exists — this must be the failure error");
        assert!(
            err.message.contains("no core is reachable"),
            "the held request carries the real reason: {err:?}"
        );
        // A request arriving after the failure errors immediately too.
        let late = tokio::time::timeout(std::time::Duration::from_secs(2), state.peer_when_bound())
            .await
            .expect("a post-failure request must not wait")
            .expect_err("still no peer");
        assert!(late.message.contains("no core is reachable"));
    }

    #[tokio::test]
    async fn add_note_and_search_via_tools() {
        use engram_core::{FakeEmbedder, SqliteStore};
        let engine = Engine::new(
            SqliteStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        );
        let server = Engram::new(engine);

        let res = server
            .add_note(Parameters(AddNoteArgs {
                version: None,
                created_at: None,
                node_type: "Decision".into(),
                title: "Adopt SQLite WAL".into(),
                body: Some("concurrent reads".into()),
                durability: None,
                session_id: None,
                code_refs: vec![],
                tags: vec![],
                project: None,
            }))
            .await
            .unwrap();
        assert!(!res.is_error.unwrap_or(false));

        let hits = server
            .search(Parameters(SearchArgs {
                detail: None,
                query: "sqlite".into(),
                types: vec![],
                limit: None,
                project: None,
                scope: None,
                ..Default::default()
            }))
            .await
            .unwrap();
        assert!(!hits.is_error.unwrap_or(false));
        // the serialized hit text should mention the node
        let text = format!("{:?}", hits.content);
        assert!(text.contains("Adopt SQLite WAL"));
    }
}

#[cfg(test)]
pub(crate) mod tool_tests {
    use super::*;
    use engram_core::{FakeEmbedder, SqliteStore};

    pub(crate) fn server() -> Engram {
        Engram::new(Engine::new(
            SqliteStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ))
    }

    fn text_of(res: &CallToolResult) -> String {
        format!("{:?}", res.content)
    }

    pub(crate) fn id_of(r: &CallToolResult) -> String {
        let t = text_of(r);
        let start = t.find("\\\"id\\\": \\\"").unwrap() + 10;
        t[start..].split("\\\"").next().unwrap().to_string()
    }

    #[tokio::test]
    async fn add_note_dates_historical_knowledge() {
        let s = server();
        let created = s
            .add_note(Parameters(AddNoteArgs {
                created_at: Some("2025-03-10".into()),
                ..note("a decision recovered from git history")
            }))
            .await
            .unwrap();
        let id = id_of(&created);
        let fetched = s
            .get_node(Parameters(GetNodeArgs {
                id,
                parents: None,
                children: None,
                project: None,
            }))
            .await
            .unwrap();
        assert!(
            text_of(&fetched).contains(&engram_core::parse_day("2025-03-10").unwrap().to_string()),
            "the node carries its original date, not the ingest date"
        );

        let bad = s
            .add_note(Parameters(AddNoteArgs {
                created_at: Some("yesterday".into()),
                ..note("undateable")
            }))
            .await;
        assert!(
            bad.is_err(),
            "an unparseable date is refused, never guessed"
        );
    }

    pub(crate) fn note(title: &str) -> AddNoteArgs {
        AddNoteArgs {
            version: None,
            created_at: None,
            node_type: "Decision".into(),
            title: title.into(),
            body: Some("shared body".into()),
            durability: None,
            session_id: None,
            code_refs: vec![],
            tags: vec![],
            project: None,
        }
    }

    #[tokio::test]
    async fn add_note_stamps_process_session_id_when_client_omits_it() {
        let s = server();
        let id = id_of(
            &s.add_note(Parameters(note("Adopt SQLite WAL")))
                .await
                .unwrap(),
        );
        let node = s.engine.lock().unwrap().get_node(&id).unwrap().unwrap();
        assert_eq!(node.session_id.as_deref(), Some(&*s.session_id));
        assert!(s.session_id.starts_with("mcp-"));
    }

    #[tokio::test]
    async fn add_note_persists_normalized_tags() {
        let s = server();
        let id = id_of(
            &s.add_note(Parameters(AddNoteArgs {
                created_at: None,
                tags: vec!["Phase 1".into(), "UI".into()],
                ..note("tagged decision")
            }))
            .await
            .unwrap(),
        );
        let node = s.engine.lock().unwrap().get_node(&id).unwrap().unwrap();
        assert_eq!(node.tags, vec!["phase-1", "ui"]);
    }

    #[tokio::test]
    async fn add_note_short_circuits_duplicates() {
        let s = server();
        let first = s
            .add_note(Parameters(note("Adopt SQLite WAL")))
            .await
            .unwrap();
        assert!(text_of(&first).contains("\\\"created\\\": true"));

        let dupe = s
            .add_note(Parameters(note("Adopt SQLite WAL")))
            .await
            .unwrap();
        let text = text_of(&dupe);
        assert!(text.contains("\\\"created\\\": false"), "got: {text}");
        assert!(text.contains("matched"));
    }

    #[tokio::test]
    async fn get_node_returns_parent_and_child_hierarchy() {
        let s = server();
        // Decision -because-> Principle (parent); Insight -about-> Decision (child).
        let principle = id_of(
            &s.add_note(Parameters(AddNoteArgs {
                version: None,
                created_at: None,
                node_type: "Principle".into(),
                title: "local first".into(),
                body: None,
                durability: None,
                session_id: None,
                code_refs: vec![],
                tags: vec![],
                project: None,
            }))
            .await
            .unwrap(),
        );
        let decision = id_of(
            &s.add_note(Parameters(note("store data in sqlite")))
                .await
                .unwrap(),
        );
        let insight = id_of(
            &s.add_note(Parameters(AddNoteArgs {
                version: None,
                created_at: None,
                node_type: "Insight".into(),
                title: "wal mode matters".into(),
                body: None,
                durability: None,
                session_id: None,
                code_refs: vec![],
                tags: vec![],
                project: None,
            }))
            .await
            .unwrap(),
        );

        s.link(Parameters(LinkArgs {
            from: decision.clone(),
            to: principle.clone(),
            edge_type: "because".into(),
            note: None,
            confidence: None,
            project: None,
        }))
        .await
        .unwrap();
        s.link(Parameters(LinkArgs {
            from: insight.clone(),
            to: decision.clone(),
            edge_type: "about".into(),
            note: None,
            confidence: None,
            project: None,
        }))
        .await
        .unwrap();

        let res = s
            .get_node(Parameters(GetNodeArgs {
                id: decision.clone(),
                parents: Some(2),
                children: Some(2),
                project: None,
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(text.contains("parents"), "got: {text}");
        assert!(text.contains("local first"), "parent node inlined: {text}");
        assert!(text.contains("children"), "got: {text}");
        assert!(
            text.contains("wal mode matters"),
            "child node inlined: {text}"
        );
        assert!(
            text.contains("trust"),
            "hierarchy nodes carry trust: {text}"
        );
    }

    #[tokio::test]
    async fn brief_tool_returns_markdown() {
        let s = server();
        s.add_note(Parameters(note("Backend in Rust")))
            .await
            .unwrap();
        let res = s
            .brief(Parameters(BriefArgs {
                max_chars: None,
                project: None,
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(text.contains("# Engram brief"));
        assert!(text.contains("Backend in Rust"));
    }

    #[tokio::test]
    async fn unlink_and_update_edge_roundtrip() {
        let s = server();
        let a = s
            .add_note(Parameters(note("first decision")))
            .await
            .unwrap();
        let b = s
            .add_note(Parameters(note(
                "second decision zzz qqq xyz totally different",
            )))
            .await
            .unwrap();
        let id_of = |r: &CallToolResult| {
            let t = text_of(r);
            let start = t.find("\\\"id\\\": \\\"").unwrap() + 10;
            t[start..].split("\\\"").next().unwrap().to_string()
        };
        let (ia, ib) = (id_of(&a), id_of(&b));

        let linked = s
            .link(Parameters(LinkArgs {
                from: ia,
                to: ib,
                edge_type: "conflicts-with".into(),
                note: None,
                confidence: None,
                project: None,
            }))
            .await
            .unwrap();
        let edge_id = id_of(&linked);

        let upd = s
            .update_edge(Parameters(UpdateEdgeArgs {
                id: edge_id.clone(),
                status: Some("resolved".into()),
                note: None,
                confidence: None,
                project: None,
            }))
            .await
            .unwrap();
        assert!(text_of(&upd).contains("\\\"ok\\\": true"));

        let gone = s
            .unlink(Parameters(IdArg {
                id: edge_id.clone(),
                project: None,
            }))
            .await
            .unwrap();
        assert!(text_of(&gone).contains("\\\"ok\\\": true"));
        assert!(
            s.unlink(Parameters(IdArg {
                id: edge_id,
                project: None,
            }))
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn timeline_tool_orders_the_replaces_chain() {
        let s = server();
        let old = id_of(
            &s.add_note(Parameters(AddNoteArgs {
                created_at: None,
                body: Some("cookie sessions".into()),
                ..note("auth v1")
            }))
            .await
            .unwrap(),
        );
        let new = id_of(
            &s.add_note(Parameters(AddNoteArgs {
                created_at: None,
                body: Some("oauth device flow".into()),
                ..note("auth v2")
            }))
            .await
            .unwrap(),
        );
        s.link(Parameters(LinkArgs {
            from: new.clone(),
            to: old.clone(),
            edge_type: "replaces".into(),
            note: Some("cookies broke on mobile".into()),
            confidence: None,
            project: None,
        }))
        .await
        .unwrap();

        let t = text_of(
            &s.timeline(Parameters(IdArg {
                id: new,
                project: None,
            }))
            .await
            .unwrap(),
        );
        let (v1, v2) = (t.find("auth v1").unwrap(), t.find("auth v2").unwrap());
        assert!(v1 < v2, "oldest first: {t}");
        assert!(t.contains("cookies broke on mobile"));
        assert!(
            s.timeline(Parameters(IdArg {
                id: "nope".into(),
                project: None,
            }))
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn list_drift_flags_missing_refs() {
        let s = server();
        s.add_note(Parameters(AddNoteArgs {
            created_at: None,
            code_refs: vec!["Cargo.toml".into(), "src/vanished.rs".into()],
            ..note("refs moved")
        }))
        .await
        .unwrap();
        let t = text_of(
            &s.list_drift(Parameters(ProjectArg { project: None }))
                .await
                .unwrap(),
        );
        assert!(t.contains("src/vanished.rs"), "got: {t}");
        assert!(
            !t.contains("Cargo.toml"),
            "existing refs are not drift: {t}"
        );
    }

    #[tokio::test]
    async fn audit_tool_pages_the_journal() {
        let s = server();
        let id = id_of(&s.add_note(Parameters(note("journaled"))).await.unwrap());
        let t = text_of(
            &s.audit(Parameters(AuditArgs {
                limit: None,
                before: None,
                entity_id: Some(id),
                project: None,
            }))
            .await
            .unwrap(),
        );
        assert!(t.contains("created"), "got: {t}");
        assert!(t.contains("journaled"));
    }

    #[tokio::test]
    async fn bulk_create_read_update_roundtrip() {
        let s = server();
        // Batch create: a Decision with a long body, a tagged Caution, and a
        // near-duplicate of the first — the dupe check must run per item.
        let created = s
            .add_notes(Parameters(AddNotesArgs {
                notes: vec![
                    AddNoteArgs {
                        created_at: None,
                        body: Some("the full body that an export must not lose".into()),
                        ..note("store data in sqlite")
                    },
                    AddNoteArgs {
                        created_at: None,
                        node_type: "Caution".into(),
                        tags: vec!["hygiene".into()],
                        ..note("never trust a relative db path")
                    },
                    AddNoteArgs {
                        created_at: None,
                        body: Some("the full body that an export must not lose".into()),
                        ..note("store data in sqlite")
                    },
                ],
            }))
            .await
            .unwrap();
        let text = text_of(&created);
        assert!(text.contains("created"), "got: {text}");
        assert!(text.contains("matched"), "per-item dupe check: {text}");

        // Full-fidelity filtered read: only Decisions, whole body included.
        let listed = s
            .list_nodes(Parameters(ListNodesArgs {
                sort: None,
                types: vec!["Decision".into()],
                status: None,
                tag: None,
                include_archived: None,
                pinned: None,
                limit: None,
                offset: None,
                project: None,
            }))
            .await
            .unwrap();
        let text = text_of(&listed);
        assert!(
            text.contains("the full body that an export must not lose"),
            "full body survives the bulk read: {text}"
        );
        assert!(
            !text.contains("relative db path"),
            "type filter holds: {text}"
        );

        // Tag filter reaches the Caution.
        let tagged = s
            .list_nodes(Parameters(ListNodesArgs {
                sort: None,
                types: vec![],
                status: None,
                tag: Some("hygiene".into()),
                include_archived: None,
                pinned: None,
                limit: None,
                offset: None,
                project: None,
            }))
            .await
            .unwrap();
        let text = text_of(&tagged);
        assert!(text.contains("relative db path"), "got: {text}");
        assert!(!text.contains("sqlite"), "got: {text}");
    }

    #[tokio::test]
    async fn update_nodes_applies_independently_and_reports_per_item() {
        let s = server();
        let id = id_of(
            &s.add_note(Parameters(note("original title")))
                .await
                .unwrap(),
        );
        let blank = |id: String| UpdateArgs {
            version: None,
            id,
            node_type: None,
            title: None,
            body: None,
            durability: None,
            status: None,
            code_refs: None,
            tags: None,
            project: None,
        };
        let res = s
            .update_nodes(Parameters(UpdateNodesArgs {
                updates: vec![
                    UpdateArgs {
                        title: Some("renamed title".into()),
                        ..blank(id.clone())
                    },
                    blank("nonexistent-id".into()),
                ],
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(
            text.contains("renamed title") || text.contains("true"),
            "got: {text}"
        );
        assert!(text.contains("false"), "bad id reported, not fatal: {text}");

        let node = s
            .get_node(Parameters(GetNodeArgs {
                id,
                parents: None,
                children: None,
                project: None,
            }))
            .await
            .unwrap();
        assert!(text_of(&node).contains("renamed title"), "patch landed");
    }

    /// A history-backed server: engine + sibling history store in a temp dir.
    fn history_server() -> (Engram, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("engram-mcp-hist-{}", engram_core::id::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("graph.tepin");
        let mut engine = Engine::with_store(
            engram_core::open_store(&db).unwrap(),
            Box::new(FakeEmbedder::default()),
        );
        engine.set_history_path(engram_core::history::history_store_path(&db));
        // Recording is opt-in (0.8.4): flip the switch the way the pane does.
        let mut cfg = engine.graph_config();
        cfg.history.enabled = true;
        engine.set_graph_config(&cfg).unwrap();
        (Engram::new(engine), dir)
    }

    fn seed_history(s: &Engram, sid: &str, texts: &[(&str, &str)]) {
        let engine = s.engine.lock().unwrap();
        let base = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        for (i, (role, text)) in texts.iter().enumerate() {
            let mut props = serde_json::Map::new();
            props.insert("role".into(), (*role).into());
            props.insert("turn".into(), (i as u64).into());
            engine
                .add_history_node(engram_core::NewNode {
                    node_type: engram_core::NodeType::parse("Message").unwrap(),
                    title: text.to_string(),
                    body: Some(text.to_string()),
                    created_at: Some(base - 100 + i as i64),
                    durability: engram_core::Durability::Stable,
                    source: engram_core::Source::Claude,
                    session_id: Some(sid.into()),
                    status: None,
                    code_refs: vec![],
                    tags: vec![],
                    version: None,
                    props: Some(props),
                })
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn search_takes_the_temporal_grammar_and_names_what_it_cannot_read() {
        let (s, dir) = history_server();
        seed_history(
            &s,
            "sess-now",
            &[("assistant", "the onnx batch width was the memory culprit")],
        );

        // A relative window the daemon resolves — the assistant never does
        // date arithmetic. The seeded messages are seconds old, so "last week"
        // holds them.
        let res = s
            .search(Parameters(SearchArgs {
                query: "onnx batch width memory".into(),
                scope: Some("history".into()),
                after: Some("last week".into()),
                ..Default::default()
            }))
            .await
            .unwrap();
        assert!(text_of(&res).contains("sess-now"), "{}", text_of(&res));

        // The same window shifted into the past excludes them.
        let res = s
            .search(Parameters(SearchArgs {
                query: "onnx batch width memory".into(),
                scope: Some("history".into()),
                before: Some("last week".into()),
                ..Default::default()
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(!text.contains("sess-now"), "window excludes it: {text}");

        // An unreadable bound is an error naming the offender and teaching the
        // grammar — never a silently dropped filter, which would answer an
        // unscoped question while looking scoped.
        let err = s
            .search(Parameters(SearchArgs {
                query: "anything".into(),
                after: Some("whenever-ish".into()),
                ..Default::default()
            }))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("whenever-ish"), "{err}");
        assert!(err.contains("last week"), "{err}");

        // So is a bad ordering.
        let err = s
            .search(Parameters(SearchArgs {
                query: "anything".into(),
                order: Some("sideways".into()),
                ..Default::default()
            }))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("chronological"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn list_sessions_scopes_to_a_window_and_says_so_when_empty() {
        let (s, dir) = history_server();
        {
            let engine = s.engine.lock().unwrap();
            for (sid, day) in [("sess-june", "2026-06-10"), ("sess-august", "2026-08-10")] {
                let mut props = serde_json::Map::new();
                props.insert("harness".into(), "claude-code".into());
                props.insert("messages".into(), 2u64.into());
                engine
                    .add_history_node(engram_core::NewNode {
                        node_type: engram_core::NodeType::parse("Session").unwrap(),
                        title: format!("{sid} opening question"),
                        body: None,
                        created_at: Some(engram_core::parse_day(day).unwrap()),
                        durability: engram_core::Durability::Stable,
                        source: engram_core::Source::Claude,
                        session_id: Some(sid.into()),
                        status: None,
                        code_refs: vec![],
                        tags: vec![],
                        version: None,
                        props: Some(props),
                    })
                    .unwrap()
                    .unwrap();
            }
        }

        let listed = |after: Option<&str>, before: Option<&str>| {
            let (after, before) = (after.map(str::to_string), before.map(str::to_string));
            let s = &s;
            async move {
                text_of(
                    &s.list_sessions(Parameters(ListSessionsArgs {
                        after,
                        before,
                        ..Default::default()
                    }))
                    .await
                    .unwrap(),
                )
            }
        };

        let text = listed(Some("2026-08-01"), None).await;
        assert!(text.contains("sess-august"), "{text}");
        assert!(!text.contains("sess-june"), "{text}");

        let text = listed(None, Some("2026-08-01")).await;
        assert!(text.contains("sess-june"), "{text}");
        assert!(!text.contains("sess-august"), "{text}");

        // An empty window says the window was empty — not that recording is
        // off, which is a different diagnosis and would send the user to the
        // wrong switch.
        let text = listed(None, Some("2020-01-01")).await;
        assert!(text.contains("No sessions in that window"), "{text}");
        assert!(!text.contains("Nothing recorded"), "{text}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn search_scope_history_returns_sectioned_hits_and_expands() {
        let (s, dir) = history_server();
        seed_history(
            &s,
            "sess-42",
            &[
                ("user", "why does the daemon leak memory"),
                ("assistant", "the onnx batch width was the memory culprit"),
                ("user", "ship it"),
            ],
        );
        let res = s
            .search(Parameters(SearchArgs {
                detail: None,
                query: "onnx batch width memory".into(),
                types: vec![],
                limit: None,
                project: None,
                scope: Some("history".into()),
                ..Default::default()
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(text.contains("history"), "{text}");
        assert!(text.contains("sess-42"), "{text}");
        assert!(
            !text.contains("\"confidence\""),
            "history scope skips curated: {text}"
        );

        let res = s
            .expand_history(Parameters(ExpandHistoryArgs {
                session: "sess-42".into(),
                turn: 1,
                window: Some(1),
                project: None,
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(text.contains("batch width"), "{text}");
        assert!(text.contains("ship it"), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn list_sessions_browses_newest_first_with_harness_filter() {
        let (s, dir) = history_server();
        // An empty layer says so instead of returning a bare [].
        let res = s
            .list_sessions(Parameters(ListSessionsArgs {
                limit: None,
                harness: None,
                project: None,
                ..Default::default()
            }))
            .await
            .unwrap();
        assert!(text_of(&res).contains("Nothing recorded"));

        {
            let engine = s.engine.lock().unwrap();
            for (sid, harness, ts) in [
                ("sess-old", "claude-code", 1_786_300_000i64),
                ("sess-new", "bob", 1_786_400_000),
            ] {
                let mut props = serde_json::Map::new();
                props.insert("harness".into(), harness.into());
                props.insert("messages".into(), 2u64.into());
                engine
                    .add_history_node(engram_core::NewNode {
                        node_type: engram_core::NodeType::parse("Session").unwrap(),
                        title: format!("{sid} opening question"),
                        body: None,
                        created_at: Some(ts),
                        durability: engram_core::Durability::Stable,
                        source: engram_core::Source::Claude,
                        session_id: Some(sid.into()),
                        status: None,
                        code_refs: vec![],
                        tags: vec![],
                        version: None,
                        props: Some(props),
                    })
                    .unwrap()
                    .unwrap();
            }
        }
        let res = s
            .list_sessions(Parameters(ListSessionsArgs {
                limit: None,
                harness: None,
                project: None,
                ..Default::default()
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(
            text.contains("sess-new") && text.contains("sess-old"),
            "{text}"
        );
        assert!(
            text.find("sess-new").unwrap() < text.find("sess-old").unwrap(),
            "newest first: {text}"
        );
        let res = s
            .list_sessions(Parameters(ListSessionsArgs {
                limit: None,
                harness: Some("bob".into()),
                project: None,
                ..Default::default()
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(
            text.contains("sess-new") && !text.contains("sess-old"),
            "{text}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn get_node_and_expand_history_carry_born_in_provenance() {
        let (s, dir) = history_server();
        // Two recorded turns at fixed times; the note parks at the assistant
        // turn's moment, so provenance resolves in the same pass.
        {
            let engine = s.engine.lock().unwrap();
            for (i, (role, text, ts)) in [
                ("user", "how should we cap the batch", 1_786_400_000i64),
                (
                    "assistant",
                    "cap it at two — measured, not guessed",
                    1_786_400_010,
                ),
            ]
            .iter()
            .enumerate()
            {
                let mut props = serde_json::Map::new();
                props.insert("role".into(), (*role).into());
                props.insert("turn".into(), (i as u64).into());
                engine
                    .add_history_node(engram_core::NewNode {
                        node_type: engram_core::NodeType::parse("Message").unwrap(),
                        title: text.to_string(),
                        body: Some(text.to_string()),
                        created_at: Some(*ts),
                        durability: engram_core::Durability::Stable,
                        source: engram_core::Source::Claude,
                        session_id: Some("sess-born".into()),
                        status: None,
                        code_refs: vec![],
                        tags: vec![],
                        version: None,
                        props: Some(props),
                    })
                    .unwrap()
                    .unwrap();
            }
        }
        let created = s
            .create_note(AddNoteArgs {
                node_type: "Decision".into(),
                title: "Batch capped at two".into(),
                body: Some("The inference batch stays at two.".into()),
                durability: None,
                tags: vec![],
                code_refs: vec![],
                session_id: None,
                version: None,
                created_at: None,
                project: None,
            })
            .unwrap();
        let note_id = created["id"].as_str().unwrap().to_string();
        {
            let engine = s.engine.lock().unwrap();
            engine.park_provenance(&note_id, 1_786_400_010);
            assert_eq!(engine.resolve_provenance().unwrap(), 1);
        }
        // get_node carries the born_in line…
        let res = s
            .get_node(Parameters(GetNodeArgs {
                id: note_id.clone(),
                parents: None,
                children: None,
                project: None,
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(text.contains("born_in"), "{text}");
        assert!(text.contains("sess-born"), "{text}");
        // …and expand_history the reverse: the notes this session left.
        let res = s
            .expand_history(Parameters(ExpandHistoryArgs {
                session: "sess-born".into(),
                turn: 1,
                window: Some(2),
                project: None,
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(text.contains("\\\"notes\\\""), "{text}"); // debug-escaped JSON
        assert!(text.contains(&note_id), "{text}");
        assert!(text.contains("Batch capped at two"), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn search_auto_falls_through_to_history_only_on_weak_verdicts() {
        let (s, dir) = history_server();
        seed_history(
            &s,
            "sess-7",
            &[("assistant", "we pinned the embedder to fp32")],
        );
        // Empty curated graph: verdict "none" — the section appears.
        let res = s
            .search(Parameters(SearchArgs {
                detail: None,
                query: "embedder pin".into(),
                types: vec![],
                limit: None,
                project: None,
                scope: None,
                ..Default::default()
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(text.contains("history"), "fall-through fired: {text}");
        assert!(text.contains("sess-7"), "{text}");
        // scope=memory never shows history, whatever the verdict.
        let res = s
            .search(Parameters(SearchArgs {
                detail: None,
                query: "embedder pin".into(),
                types: vec![],
                limit: None,
                project: None,
                scope: Some("memory".into()),
                ..Default::default()
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(
            !text.contains("sess-7"),
            "memory scope stays curated: {text}"
        );
        // The knob: search_fallthrough=false silences the section.
        {
            let engine = s.engine.lock().unwrap();
            let mut cfg = engine.graph_config();
            cfg.history.search_fallthrough = false;
            engine.set_graph_config(&cfg).unwrap();
        }
        let res = s
            .search(Parameters(SearchArgs {
                detail: None,
                query: "embedder pin".into(),
                types: vec![],
                limit: None,
                project: None,
                scope: None,
                ..Default::default()
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(
            !text.contains("sess-7"),
            "knob off = no fall-through: {text}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn search_snippets_use_brackets_not_sentinels() {
        let s = server();
        s.add_note(Parameters(note("sentinel roundtrip check")))
            .await
            .unwrap();
        let res = s
            .search(Parameters(SearchArgs {
                detail: None,
                query: "sentinel".into(),
                types: vec![],
                limit: None,
                project: None,
                scope: None,
                ..Default::default()
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(text.contains('['), "brackets for assistants: {text}");
        assert!(
            !text.contains('\u{e000}') && !text.contains('\u{e001}'),
            "no raw sentinels leak over MCP: {text}"
        );
    }
}

#[cfg(test)]
mod project_tests {
    use super::*;
    use engram_core::{FakeEmbedder, SqliteStore};

    fn server() -> Engram {
        Engram::new(Engine::new(
            SqliteStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ))
    }

    #[tokio::test]
    async fn all_writes_are_refused_with_the_home_pointer() {
        let s = server();
        let err = s
            .add_note(Parameters(AddNoteArgs {
                version: None,
                created_at: None,
                node_type: "Decision".into(),
                title: "fan out".into(),
                body: None,
                durability: None,
                session_id: None,
                code_refs: vec![],
                tags: vec![],
                project: Some("all".into()),
            }))
            .await
            .unwrap_err();
        assert!(err.message.contains("home"), "got: {}", err.message);
    }

    #[tokio::test]
    async fn unknown_project_selector_is_invalid_params() {
        let s = server();
        let err = s
            .search(Parameters(SearchArgs {
                detail: None,
                query: "anything".into(),
                types: vec![],
                limit: None,
                project: Some("definitely-not-registered-xyz".into()),
                scope: None,
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert!(
            err.message.contains("definitely-not-registered-xyz"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn list_projects_reports_the_current_project() {
        let s = server();
        let t = format!("{:?}", s.list_projects().await.unwrap().content);
        assert!(t.contains("projects"), "got: {t}");
        assert!(t.contains("current"), "got: {t}");
    }
}

#[cfg(test)]
mod suspect_tests {
    use super::*;
    use engram_core::{FakeEmbedder, SqliteStore};

    #[tokio::test]
    async fn brief_lists_suspects_and_resolve_judges_them() {
        let s = Engram::new(Engine::new(
            SqliteStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ));
        let mk = |t: &str, ty: &str| AddNoteArgs {
            version: None,
            created_at: None,
            node_type: ty.into(),
            title: t.into(),
            body: None,
            durability: None,
            session_id: None,
            code_refs: vec![],
            tags: vec![],
            project: None,
        };
        s.add_note(Parameters(mk("cache invalidation via ttl", "Decision")))
            .await
            .unwrap();
        // Cross-type twin: dodges the duplicate short-circuit, lands as a suspect.
        s.add_note(Parameters(mk("cache invalidation via ttl", "Caution")))
            .await
            .unwrap();

        let listed = format!(
            "{:?}",
            s.list_suspects(Parameters(ProjectArg { project: None }))
                .await
                .unwrap()
                .content
        );
        assert!(listed.contains("suspects"), "got: {listed}");
        let brief = s
            .brief(Parameters(BriefArgs {
                max_chars: None,
                project: None,
            }))
            .await
            .unwrap();
        let brief_text = format!("{:?}", brief.content);
        assert!(
            brief_text.contains("Suspected conflicts"),
            "got: {brief_text}"
        );

        let sid = s.engine.lock().unwrap().suspects().unwrap().remove(0).id;
        let resolved = s
            .resolve_suspect(Parameters(ResolveSuspectArgs {
                id: sid,
                verdict: "conflict".into(),
                project: None,
            }))
            .await
            .unwrap();
        let text = format!("{:?}", resolved.content);
        assert!(text.contains("conflicts-with"), "got: {text}");
    }

    #[tokio::test]
    async fn write_response_surfaces_freshly_queued_suspects() {
        let s = Engram::new(Engine::new(
            SqliteStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ));
        let mk = |t: &str, ty: &str| AddNoteArgs {
            version: None,
            created_at: None,
            node_type: ty.into(),
            title: t.into(),
            body: None,
            durability: None,
            session_id: None,
            code_refs: vec![],
            tags: vec![],
            project: None,
        };
        let first = s
            .add_note(Parameters(mk(
                "retry queue drains on reconnect",
                "Decision",
            )))
            .await
            .unwrap();
        let first_text = format!("{:?}", first.content);
        assert!(
            !first_text.contains("suspects"),
            "nothing to suspect yet: {first_text}"
        );

        // Cross-type twin: dodges the duplicate short-circuit, queues a
        // suspect — which the WRITE RESPONSE itself must now surface.
        let second = s
            .add_note(Parameters(mk("retry queue drains on reconnect", "Caution")))
            .await
            .unwrap();
        let text = format!("{:?}", second.content);
        assert!(text.contains("suspects"), "got: {text}");
        assert!(
            text.contains("action_required") && text.contains("resolve_suspect"),
            "the response tells the writer what to do: {text}"
        );
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use engram_core::{FakeEmbedder, SqliteStore};
    use rmcp::model::CallToolRequestParams;

    /// The §7C thin-client chain, end to end and in-process: a daemon-style
    /// axum server hosting /mcp, a direct streamable-HTTP client against it,
    /// and a full stdio-shaped bridge (Passthrough over an in-memory duplex)
    /// relaying a second client through it.
    #[tokio::test]
    async fn streamable_http_daemon_and_stdio_bridge_end_to_end() {
        let engine = Engine::new(
            SqliteStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        );
        let hub = Arc::new(Hub::single(engine));
        let app = axum::Router::new().route_service("/mcp", streamable_http_service(hub.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let url = format!("http://{addr}/mcp");

        // Direct client: handshake + a real tool call against the endpoint.
        let direct = ()
            .serve(rmcp::transport::StreamableHttpClientTransport::from_uri(
                url.clone(),
            ))
            .await
            .unwrap();
        let tools = direct.peer().list_all_tools().await.unwrap();
        assert!(tools.iter().any(|t| t.name == "brief"));
        assert!(tools.iter().any(|t| t.name == "add_note"));
        let noted = direct
            .peer()
            .call_tool(
                CallToolRequestParams::new("add_note").with_arguments(
                    serde_json::json!({
                        "type": "Decision",
                        "title": "served over the daemon transport",
                        "durability": "stable"
                    })
                    .as_object()
                    .cloned()
                    .unwrap(),
                ),
            )
            .await
            .unwrap();
        assert_ne!(noted.is_error, Some(true));

        // The bridge: stdio-shaped duplex → Passthrough → the same endpoint,
        // wired through the real BridgeState::connect path (Fixed shape).
        let (bound, _bound_rx) = tokio::sync::watch::channel(0u64);
        let (exit, _exit_rx) = tokio::sync::mpsc::channel(4);
        let state = Arc::new(BridgeState {
            http: reqwest::Client::new(),
            slot: std::sync::Mutex::new(None),
            bound,
            lease: std::sync::Mutex::new(None),
            client_name: std::sync::Mutex::new(None),
            exit,
            failed: std::sync::Mutex::new(None),
        });
        let info = state.connect(&url, None).await.unwrap();
        let proxy = Passthrough {
            state: state.clone(),
            info,
            roots: None,
        };
        let (bridge_io, client_io) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            let server = proxy.serve(bridge_io).await.unwrap();
            let _ = server.waiting().await;
        });
        let bridged = ().serve(client_io).await.unwrap();
        assert_eq!(
            bridged.peer().peer_info().unwrap().server_info.name,
            "engram",
            "the bridge mirrors the daemon's identity"
        );
        let hits = bridged
            .peer()
            .call_tool(
                CallToolRequestParams::new("search").with_arguments(
                    serde_json::json!({ "query": "daemon transport" })
                        .as_object()
                        .cloned()
                        .unwrap(),
                ),
            )
            .await
            .unwrap();
        assert_ne!(hits.is_error, Some(true));
        let text = format!("{:?}", hits.content);
        assert!(
            text.contains("served over the daemon transport"),
            "a write through the direct client is visible through the bridge: {text}"
        );
    }
}

#[cfg(test)]
mod scoped_transport_tests {
    use super::*;
    use engram_core::{FakeEmbedder, SqliteStore, registry};
    use rmcp::model::CallToolRequestParams;

    /// v0.6.2: a session on /projects/{id}/mcp treats that project as current
    /// — the repo-bound AI side of "one core, one pane".
    #[tokio::test]
    async fn scoped_mcp_endpoint_binds_sessions_to_their_project() {
        let tmp = std::env::temp_dir().join(format!("engram-mcp-scope-{}", std::process::id()));
        let beta_root = tmp.join("beta");
        std::fs::create_dir_all(beta_root.join(".engram")).unwrap();
        unsafe { std::env::set_var("ENGRAM_HOME", tmp.join("home")) };
        let beta_db = beta_root.join(".engram/graph.db");
        registry::register(&beta_root, &beta_db).unwrap();
        {
            let beta = Engine::new(
                SqliteStore::open(&beta_db).unwrap(),
                Box::new(FakeEmbedder::default()),
            );
            beta.add_node(engram_core::NewNode {
                version: None,
                created_at: None,
                node_type: engram_core::NodeType::Decision,
                title: "beta owns this decision".into(),
                body: None,
                durability: engram_core::Durability::Stable,
                source: engram_core::Source::Claude,
                session_id: None,
                status: None,
                code_refs: vec![],
                tags: vec![],
                props: None,
            })
            .unwrap();
        }

        let alpha = Engine::new(
            SqliteStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        );
        let factory: engram_core::EngineFactory = Box::new(|db| {
            Ok(Engine::new(
                SqliteStore::open(db)?,
                Box::new(FakeEmbedder::default()),
            ))
        });
        let hub = Arc::new(Hub::new(Arc::new(Mutex::new(alpha)), None, Some(factory)));

        let app = axum::Router::new()
            .route_service("/mcp", streamable_http_service(hub.clone()))
            .route_service(
                "/projects/beta/mcp",
                streamable_http_service_for(hub.clone(), "beta".into()),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let scoped = ()
            .serve(rmcp::transport::StreamableHttpClientTransport::from_uri(
                format!("http://{addr}/projects/beta/mcp"),
            ))
            .await
            .unwrap();
        let hits = scoped
            .peer()
            .call_tool(
                CallToolRequestParams::new("search").with_arguments(
                    serde_json::json!({ "query": "beta owns decision" })
                        .as_object()
                        .cloned()
                        .unwrap(),
                ),
            )
            .await
            .unwrap();
        let text = format!("{:?}", hits.content);
        assert!(
            text.contains("beta owns this decision"),
            "scoped session searches ITS project by default: {text}"
        );

        // 0.8.8 regression guard: the SESSION's binding — not the hub's own
        // current project — is what an unscoped `brief` renders and what
        // `list_projects` calls current. Both used to read `hub.current`,
        // which was invisibly right while the daemon launched inside the repo
        // it served and became always-wrong once the machine core became a
        // dedicated home-rooted process: every bound session was handed the
        // home graph's cold-start brief and told it was sitting in `home`.
        let brief = scoped
            .peer()
            .call_tool(CallToolRequestParams::new("brief"))
            .await
            .unwrap();
        let text = format!("{:?}", brief.content);
        assert!(
            text.contains("beta owns this decision"),
            "an unscoped brief on a bound session briefs THAT project: {text}"
        );

        // …and a `project` argument accepts the project's DIRECTORY, so a
        // caller holding a folder (a hook, a bridge's cwd) never has to map
        // it to a name first.
        let by_dir = scoped
            .peer()
            .call_tool(
                CallToolRequestParams::new("brief").with_arguments(
                    serde_json::json!({ "project": beta_root.display().to_string() })
                        .as_object()
                        .cloned()
                        .unwrap(),
                ),
            )
            .await
            .unwrap();
        let by_dir = format!("{:?}", by_dir.content);
        assert!(
            by_dir.contains("beta owns this decision"),
            "a directory names its project: {by_dir}"
        );

        let roster = scoped
            .peer()
            .call_tool(CallToolRequestParams::new("list_projects"))
            .await
            .unwrap();
        let text = format!("{:?}", roster.content);
        // One pretty-printed JSON object per project, so splitting on the
        // brace puts each row's name beside its own `current` flag.
        let row = |name: &str| {
            text.split('{')
                .find(|r| r.contains(&format!("\\\"name\\\": \\\"{name}\\\"")))
                .unwrap_or_else(|| panic!("no {name} row in {text}"))
                .to_string()
        };
        assert!(
            row("beta").contains("\\\"current\\\": true"),
            "the bound project is the current one: {text}"
        );
        assert!(
            row("home").contains("\\\"current\\\": false"),
            "the core's own launch graph is not this session's project: {text}"
        );

        let unscoped = ()
            .serve(rmcp::transport::StreamableHttpClientTransport::from_uri(
                format!("http://{addr}/mcp"),
            ))
            .await
            .unwrap();
        let hits = unscoped
            .peer()
            .call_tool(
                CallToolRequestParams::new("search").with_arguments(
                    serde_json::json!({ "query": "beta owns decision" })
                        .as_object()
                        .cloned()
                        .unwrap(),
                ),
            )
            .await
            .unwrap();
        let text = format!("{:?}", hits.content);
        assert!(
            !text.contains("beta owns this decision"),
            "the hub's current project stays its own graph: {text}"
        );

        unsafe { std::env::remove_var("ENGRAM_HOME") };
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[cfg(test)]
mod push_and_params_tests {
    use super::tool_tests::{id_of, note, server};
    use super::*;
    use engram_core::{FakeEmbedder, SqliteStore};

    fn two_sessions() -> (Arc<Hub>, Engram, Engram) {
        let hub = Arc::new(Hub::single(Engine::new(
            SqliteStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        )));
        (
            hub.clone(),
            Engram::with_hub(hub.clone()),
            Engram::with_hub(hub),
        )
    }

    fn body_of(r: &CallToolResult) -> String {
        format!("{:?}", r.content)
    }

    #[tokio::test]
    async fn judged_conflicts_push_into_other_live_sessions() {
        let (_hub, writer, reader) = two_sessions();
        let a = writer
            .add_note(Parameters(AddNoteArgs {
                created_at: None,
                ..note("zzz retry policy: exponential backoff")
            }))
            .await
            .unwrap();
        let b = writer
            .add_note(Parameters(AddNoteArgs {
                created_at: None,
                ..note("qqq retry policy: never retry anything")
            }))
            .await
            .unwrap();
        let (a_id, b_id) = (id_of(&a), id_of(&b));

        // Reader's calls are clean before any judgment.
        let calm = reader
            .search(Parameters(SearchArgs {
                detail: None,
                query: "retry policy".into(),
                types: vec![],
                limit: None,
                project: None,
                scope: None,
                ..Default::default()
            }))
            .await
            .unwrap();
        assert!(!body_of(&calm).contains("MEMORY ALERT"));

        // The writer records the contradiction…
        writer
            .link(Parameters(LinkArgs {
                from: b_id,
                to: a_id,
                edge_type: "conflicts-with".into(),
                note: None,
                confidence: None,
                project: None,
            }))
            .await
            .unwrap();

        // …the reader's NEXT tool call carries the push, titles included.
        let alerted = reader
            .search(Parameters(SearchArgs {
                detail: None,
                query: "retry policy".into(),
                types: vec![],
                limit: None,
                project: None,
                scope: None,
                ..Default::default()
            }))
            .await
            .unwrap();
        let text = body_of(&alerted);
        assert!(
            text.contains("MEMORY ALERT"),
            "push rides the next response"
        );
        assert!(text.contains("qqq retry policy"), "alerts carry titles");

        // Delivered once: the reader's following call is clean again.
        let after = reader
            .brief(Parameters(BriefArgs {
                max_chars: Some(500),
                project: None,
            }))
            .await
            .unwrap();
        assert!(!body_of(&after).contains("MEMORY ALERT"));

        // And the writer never gets its own judgment echoed back.
        let writer_next = writer
            .search(Parameters(SearchArgs {
                detail: None,
                query: "retry policy".into(),
                types: vec![],
                limit: None,
                project: None,
                scope: None,
                ..Default::default()
            }))
            .await
            .unwrap();
        assert!(
            !body_of(&writer_next).contains("MEMORY ALERT"),
            "own-write suppression holds"
        );
    }

    #[tokio::test]
    async fn search_detail_tiers_shape_the_response() {
        let s = server();
        s.add_note(Parameters(AddNoteArgs {
            created_at: None,
            body: Some("a body with plenty of retrieval detail inside it".into()),
            ..note("progressive disclosure subject")
        }))
        .await
        .unwrap();
        let call = |detail: Option<&str>| {
            let s = &s;
            let detail = detail.map(str::to_string);
            async move {
                body_of(
                    &s.search(Parameters(SearchArgs {
                        detail,
                        query: "progressive disclosure".into(),
                        types: vec![],
                        limit: None,
                        project: None,
                        scope: None,
                        ..Default::default()
                    }))
                    .await
                    .unwrap(),
                )
            }
        };
        let compact = call(Some("compact")).await;
        assert!(
            !compact.contains("snippet"),
            "compact strips snippets: {compact}"
        );
        assert!(!compact.contains("neighbors"), "compact strips neighbors");
        assert!(compact.contains("progressive disclosure subject"));
        let full = call(Some("full")).await;
        assert!(
            full.contains("plenty of retrieval detail"),
            "full attaches bodies: {full}"
        );
        let bad = s
            .search(Parameters(SearchArgs {
                detail: Some("verbose".into()),
                query: "x".into(),
                types: vec![],
                limit: None,
                project: None,
                scope: None,
                ..Default::default()
            }))
            .await;
        assert!(bad.is_err(), "unknown detail level is refused");
    }

    #[tokio::test]
    async fn list_nodes_sorts_by_connectivity() {
        let s = server();
        let hubnode = s
            .add_note(Parameters(AddNoteArgs {
                created_at: None,
                ..note("zzz the well connected hub node")
            }))
            .await
            .unwrap();
        let spoke = s
            .add_note(Parameters(AddNoteArgs {
                created_at: None,
                ..note("qqq a spoke node with one link")
            }))
            .await
            .unwrap();
        s.add_note(Parameters(AddNoteArgs {
            created_at: None,
            ..note("xxx an island nobody linked or tagged")
        }))
        .await
        .unwrap();
        s.link(Parameters(LinkArgs {
            from: id_of(&spoke),
            to: id_of(&hubnode),
            edge_type: "builds-on".into(),
            note: None,
            confidence: None,
            project: None,
        }))
        .await
        .unwrap();

        let order = |sort: &str| {
            let s = &s;
            let sort = sort.to_string();
            async move {
                body_of(
                    &s.list_nodes(Parameters(ListNodesArgs {
                        sort: Some(sort),
                        types: vec![],
                        status: None,
                        tag: None,
                        include_archived: None,
                        pinned: None,
                        offset: None,
                        limit: None,
                        project: None,
                    }))
                    .await
                    .unwrap(),
                )
            }
        };
        let least = order("least-connected").await;
        assert!(
            least.find("island nobody").unwrap() < least.find("well connected hub").unwrap(),
            "least-connected surfaces the islands first"
        );
        let most = order("most-connected").await;
        assert!(
            most.find("well connected hub").unwrap() < most.find("island nobody").unwrap(),
            "most-connected surfaces the hubs first"
        );
    }
}
