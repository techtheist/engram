//! Per-session state: the journal bracket, the binding and its ladder rung.

use crate::*;

/// Journal bracket for one MCP session (activity audit): `mcp_session_started`
/// on construction, `mcp_session_ended` when the LAST clone of the session's
/// server drops (the server is `Clone`, so the bracket rides an `Arc`).
/// Best-effort by design — a session must never fail over its own trace.
pub(crate) struct SessionTrace {
    pub(crate) engine: Arc<Mutex<Engine>>,
    pub(crate) session_id: Arc<str>,
}

pub(crate) const VALIDATION_MIN_INTERVAL_SECS: i64 = 60;

/// The label every history section carries — raw dialogue is a different
/// register from curated memory and must never read as equivalent.
pub(crate) const HISTORY_SECTION_NOTE: &str = "From session history — raw recorded dialogue, not curated \
     memory. Snippets only; call expand_history(session, turn) to read the surrounding \
     exchange before relying on one.";

impl SessionTrace {
    pub(crate) fn start(
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

    pub(crate) fn emit(&self, action: &str, note: Option<String>) {
        let mut engine = self.engine.locked();
        engine.set_audit_origin(engram_core::AuditOrigin::mcp(self.session_id.to_string()));
        let _ = engine.audit_activity(action, note);
    }

    /// Run the full validation in the background so connects stay instant.
    /// Rate-limited per graph (the engine keeps the clock); skipped on fake
    /// embeddings, whose vectors would queue noise suspects.
    pub(crate) fn validate(&self) {
        let engine = self.engine.clone();
        let session = self.session_id.clone();
        std::thread::spawn(move || {
            // Decide and attribute under one short lock, then let the pass
            // take and release it per step: holding it for the whole scan
            // starved every request on this graph.
            {
                let mut engine = engine.locked();
                if engine.embeddings_are_fake()
                    || !engine.validation_due(VALIDATION_MIN_INTERVAL_SECS)
                {
                    return;
                }
                engine.set_audit_origin(engram_core::AuditOrigin::mcp(session.to_string()));
            }
            let _ = engram_core::Engine::validate_graph_shared(&engine);
        });
    }
}

impl Drop for SessionTrace {
    fn drop(&mut self) {
        self.emit("mcp_session_ended", None);
        self.validate();
    }
}

/// The session's live binding: the engine an omitted `project` means, the
/// selector reported as "current", and the lifecycle trace. One lock, because a
/// scoped `brief` swaps all three at once.
pub(crate) struct Binding {
    pub(crate) engine: Arc<Mutex<Engine>>,
    /// Resolved project id when bound (`/projects/{id}/mcp` or a scoped
    /// `brief`); `None` = the launch project.
    pub(crate) bound: Option<Arc<str>>,
    /// Session lifecycle journal rows in the bound graph; the old graph gets
    /// its `mcp_session_ended` when a rebind drops this.
    pub(crate) trace: Arc<SessionTrace>,
    /// Which rung of the binding ladder produced this binding (issue #11).
    /// A bridge announces its rung in the upstream initialize; a scoped
    /// `brief` overwrites it with [`BoundBy::Brief`].
    pub(crate) bound_by: BoundBy,
    /// The end client's `clientInfo.name` as the bridge relayed it
    /// ("Windsurf", "claude-code", …) — for teaching errors and the census.
    pub(crate) client: Option<String>,
    /// Writes this session has made (through any tool): the binding note
    /// on a fallback-bound session rides only the first one.
    pub(crate) writes: u32,
}

/// How a session got bound to its project (issue #11). The bridge relays its
/// rung as `bound_by=<rung>` in `clientInfo.description`; the core records it
/// and gates writes on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundBy {
    /// The session's route decided it and no bridge said more (a direct
    /// HTTP MCP client, a pre-0.9.8 bridge, or tests).
    Route,
    /// An explicit `--db` on the bridge.
    Db,
    /// The client's MCP roots.
    Roots,
    /// The bridge's launch directory hosts the project.
    Cwd,
    /// The machine-level default-agent-project setting.
    DefaultProject,
    /// The home graph, because nothing else could bind: the client answered
    /// no roots and the launch directory can't host a project. The ONLY rung
    /// that refuses writes until the agent binds the session itself.
    Home,
    /// The home graph because the folder isn't an Engram project and the bridge
    /// runs `--wired-only` (e.g. the Claude Code plugin). Refuses project writes
    /// like [`BoundBy::Home`], but teaches wiring the folder.
    Unwired,
    /// The agent rebound the session with a scoped `brief`.
    Brief,
}

impl BoundBy {
    pub fn as_str(self) -> &'static str {
        match self {
            BoundBy::Route => "route",
            BoundBy::Db => "db",
            BoundBy::Roots => "roots",
            BoundBy::Cwd => "cwd",
            BoundBy::DefaultProject => "default-project",
            BoundBy::Home => "home",
            BoundBy::Unwired => "unwired",
            BoundBy::Brief => "brief",
        }
    }

    /// The inverse of [`as_str`](Self::as_str); unknown tokens read as
    /// `Route` — a newer bridge's vocabulary never breaks an older core.
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "db" => BoundBy::Db,
            "roots" => BoundBy::Roots,
            "cwd" => BoundBy::Cwd,
            "default-project" => BoundBy::DefaultProject,
            "home" => BoundBy::Home,
            "unwired" => BoundBy::Unwired,
            "brief" => BoundBy::Brief,
            _ => BoundBy::Route,
        }
    }

    /// The `bound_by=<rung>` token a bridge puts in its upstream
    /// `clientInfo.description`.
    pub fn announce(self) -> String {
        format!("bound_by={}", self.as_str())
    }

    /// Reads the announced rung out of a `clientInfo.description`, if the
    /// client is an engram bridge that announced one.
    pub fn from_description(description: Option<&str>) -> Option<Self> {
        description?
            .split(';')
            .map(str::trim)
            .find_map(|part| part.strip_prefix("bound_by="))
            .map(Self::parse)
    }

    /// A rung the agent did not choose and the user did not configure —
    /// the session is in the home graph only because nothing else worked.
    pub fn refuses_writes(self) -> bool {
        matches!(self, BoundBy::Home | BoundBy::Unwired)
    }
}

/// Removes the session from the hub's live census when the last clone drops.
pub(crate) struct SessionRegistration {
    pub(crate) hub: Arc<Hub>,
    pub(crate) session_id: Arc<str>,
}

impl Drop for SessionRegistration {
    fn drop(&mut self) {
        self.hub.session_end(&self.session_id);
    }
}

#[derive(Clone)]
pub struct Engram {
    /// The live binding — see [`Binding`]. Clones share it: rmcp hands out
    /// clones per request, and a brief-rebind on one must move them all.
    pub(crate) binding: Arc<std::sync::RwLock<Binding>>,
    /// The multi-project hub. Single-project constructions get a
    /// factory-less hub, so cross-project selectors fail with a clear message.
    pub(crate) hub: Arc<Hub>,
    /// Fallback session id when the client omits one: minted once per server
    /// process, which over stdio is one Claude session. Superseded by the
    /// transport session id after the streamable-HTTP migration.
    pub(crate) session_id: Arc<str>,
    /// The mid-session conflict push (v0.6.3): judged contradictions from any
    /// session or project land here and ride out on this session's next tool
    /// response.
    pub(crate) conflicts: Arc<Mutex<engram_core::ConflictFeed>>,
    /// Census lifetime guard; ends when the last clone drops.
    pub(crate) _registration: Arc<SessionRegistration>,
}
