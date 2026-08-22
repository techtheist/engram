//! Thin axum API over `engram_core::Hub`: CRUD + hybrid search + the
//! whole-graph read the pane renders, plus a Server-Sent-Events stream that
//! pushes every mutation so the pane updates live (PLAN §6B).
//!
//! Project scoping (PLAN §7C): every graph route also exists under
//! `/projects/{id-or-name}/…` — a rewrite layer strips the prefix and stashes
//! the selector, so one handler set serves both forms. The bare routes are
//! the launch project (back-compat for existing panes and plugins). `home`
//! addresses the user-level home graph; registry meta ops live at
//! `/projects`.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::Json;
use axum::Router;
use axum::extract::{FromRequestParts, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::middleware;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use engram_core::{
    AnsweredHint, AuditOrigin, AuditPage, AuditSweep, ChangeEvent, ClaimReport, Drift, Edge,
    EdgePatch, EdgeType, Engine, Error, ExportGraph, Hub, ImportSummary, NewEdge, NewNode,
    NliAgreement, Node, NodePatch, NodeType, ProjectInfo, SuspectVerdict, SuspectView, TagStat,
    TimelineEntry, registry,
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::cors::CorsLayer;

type EventMap = Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>;

/// Shared server state: the hub (each engine serialized behind its own
/// `Mutex`, since a SQLite connection is `!Sync`) and one live-update
/// broadcast channel per project.
/// The daemon-side hands that `/models` needs (PLAN §7A model selection):
/// describing the current selection and applying a new one — downloading
/// files, loading the model, swapping it into every open engine, re-embedding
/// where the embedding identity changed. Implemented by the CLI (downloads
/// are its job; this crate stays curl-free), absent in library embeddings.
pub trait ModelAdmin: Send + Sync {
    fn describe(&self) -> serde_json::Value;
    /// `request`: `{"role": "embedding"|"reranker"|"nli", "preset": name}` or
    /// `{"role": …, "custom": ModelSpec}`.
    fn apply(&self, request: serde_json::Value) -> engram_core::Result<serde_json::Value>;
}

/// Skill installation hands (PLAN §7D teaching surface): generate the
/// capture skill from a graph's ontology and write it into the project.
/// Implemented CLI-side, where the canonical skill texts are embedded.
pub trait SkillAdmin: Send + Sync {
    /// Install the skill for `cfg` under `repo_root`. Returns
    /// `{installed: true, path, generated, variant}` on a write, or
    /// `{installed: false, symlink: true, path, target, note}` when the
    /// skill dir is a symlink into a source tree (deliberate sourcing —
    /// reported, left untouched, never written through).
    fn install(
        &self,
        repo_root: &std::path::Path,
        cfg: &engram_core::GraphConfig,
        variant: &str,
    ) -> engram_core::Result<serde_json::Value>;
}

pub struct AppState {
    hub: Arc<Hub>,
    events: EventMap,
    /// The database this daemon was launched on, reported by `/health` so a
    /// client that discovered a port can verify it belongs to *this* repo.
    db_path: Option<String>,
    /// Daemon start time, for `/system`'s uptime.
    started: std::time::Instant,
    /// Model selection hands, when a daemon provides them.
    model_admin: Option<Arc<dyn ModelAdmin>>,
    /// Skill installation hands, when a daemon provides them.
    skill_admin: Option<Arc<dyn SkillAdmin>>,
    /// The machine core's process identity + client census + shutdown
    /// trigger. Only the core daemon sets this; without it the census and
    /// `/shutdown` routes answer 404.
    runtime: Option<Arc<CoreRuntime>>,
    /// Per-session already-injected node ids for /refs/match — the ambient
    /// hook's dedupe memory (bounded; evicted wholesale when it grows).
    refs_seen: Mutex<HashMap<String, std::collections::HashSet<String>>>,
}

// ---- process census (process-model refactor) -----------------------------

/// One connected client's lease. Bridges register on connect, renew alongside
/// their heartbeat, and deregister on clean exit; a lease that stops renewing
/// expires (crashed client) and is pruned lazily on read.
#[derive(Clone, Serialize)]
pub struct ClientLease {
    pub lease_id: String,
    pub pid: u32,
    pub kind: String,
    /// The MCP client behind the bridge (`clientInfo.name` from its stdio
    /// initialize, e.g. "claude-code", "mcp-go"). Additive since the
    /// default-agent-project work: bridges send it once they know it (a
    /// fixed-target bridge registers before its client's initialize, so the
    /// name arrives on the first renewal); absent rows stay valid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// Registered project id owning `root`, when the registry knows it.
    pub project: Option<String>,
    pub root: String,
    pub connected_at: i64,
    pub last_seen: i64,
}

/// A lease this old without a renewal belongs to a dead client. Renewals ride
/// the bridges' 15s heartbeat, so 45s means three missed beats.
/// (`ENGRAM_LEASE_TTL_SECS` overrides for tests.)
fn lease_ttl_secs() -> i64 {
    std::env::var("ENGRAM_LEASE_TTL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(45)
}

/// Idle-unload bookkeeping (the strict-spec Decision): the three clocks the
/// core's idle checker compares against `ENGRAM_IDLE_UNLOAD_SECS`, plus the
/// residency state `/system.models_state` reports. Lives here because two of
/// the clocks are stamped from HTTP-side code (the activity layer, the lease
/// handlers) while the third is stamped by the CLI's reload-capable model
/// slots — one shared tracker keeps them coherent.
///
/// What counts as HTTP activity is decided by [`IdleTracker::stamp_http_request`]:
/// every request EXCEPT observability and process-management endpoints —
/// `/health` (plugins poll it every 10s), `/system` (status/pane must be able
/// to watch the idle badge without defeating it), `/clients*` (lease
/// register/ping/delete govern idleness through the lease clock instead),
/// `/shutdown`, and the `/projects` registry meta ops (`engram-alpha status`
/// reads the list). Scoped forms (`/projects/{id}/health`) are normalized
/// first. Everything that reads or writes graph data stamps.
pub struct IdleTracker {
    /// Unix seconds of the last HTTP request that counts as activity.
    last_http: std::sync::atomic::AtomicI64,
    /// Unix seconds of the last model use (stamped by the CLI's lazy slots on
    /// every embed/rank/judge — background sweeps extend this without being
    /// HTTP activity, so an in-flight sweep is never cut mid-work).
    last_model_use: std::sync::atomic::AtomicI64,
    /// Unix seconds a live bridge lease was last seen (register, ping renewal,
    /// or the idle checker observing a non-empty census).
    last_lease_seen: std::sync::atomic::AtomicI64,
    /// Unix seconds the models were idle-unloaded; 0 = loaded.
    unloaded_since: std::sync::atomic::AtomicI64,
    /// Unix seconds the current residency began (start, or last reload).
    loaded_since: std::sync::atomic::AtomicI64,
    /// Monotonic count of graph mutations (curated ChangeEvents + harvester
    /// writes) — the librarian's dirty flag: when models are unloaded and
    /// this hasn't moved since its last completed pass, the 6h tick skips
    /// instead of silently reloading models to sweep an unchanged graph.
    graph_writes: std::sync::atomic::AtomicU64,
}

impl Default for IdleTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl IdleTracker {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicI64, AtomicU64};
        let now = engram_core::now();
        Self {
            last_http: AtomicI64::new(now),
            last_model_use: AtomicI64::new(now),
            last_lease_seen: AtomicI64::new(now),
            unloaded_since: AtomicI64::new(0),
            loaded_since: AtomicI64::new(now),
            graph_writes: AtomicU64::new(0),
        }
    }

    /// Stamp `last_http_activity` for a request path, unless the path is one
    /// of the exempt observability/process endpoints (see the type docs).
    pub fn stamp_http_request(&self, path: &str) {
        if Self::counts_as_http_activity(path) {
            self.last_http.store(engram_core::now(), ORD);
        }
    }

    /// The exemption rule, split out so it is testable and documented in one
    /// place. `path` is the raw request path (pre scope-rewrite).
    pub fn counts_as_http_activity(path: &str) -> bool {
        let core = if let Some(rest) = path.strip_prefix("/projects/") {
            match rest.split_once('/') {
                // Scoped graph route: /projects/x/search → "search".
                Some((_, tail)) if !tail.is_empty() => tail,
                // /projects/{id} registry meta op.
                _ => return false,
            }
        } else if path == "/projects" {
            return false;
        } else {
            path.trim_start_matches('/')
        };
        let head = core.split('/').next().unwrap_or("");
        !matches!(head, "health" | "system" | "shutdown" | "clients")
    }

    pub fn touch_model_use(&self) {
        self.last_model_use.store(engram_core::now(), ORD);
    }

    pub fn touch_lease(&self) {
        self.last_lease_seen.store(engram_core::now(), ORD);
    }

    pub fn note_graph_write(&self) {
        self.graph_writes.fetch_add(1, ORD);
    }

    pub fn graph_writes(&self) -> u64 {
        self.graph_writes.load(ORD)
    }

    pub fn last_http_activity(&self) -> i64 {
        self.last_http.load(ORD)
    }

    pub fn last_model_use(&self) -> i64 {
        self.last_model_use.load(ORD)
    }

    pub fn last_lease_seen(&self) -> i64 {
        self.last_lease_seen.load(ORD)
    }

    pub fn is_unloaded(&self) -> bool {
        self.unloaded_since.load(ORD) > 0
    }

    pub fn mark_unloaded(&self, now: i64) {
        self.unloaded_since.store(now, ORD);
    }

    /// Back to resident (startup completion, lazy reload, hot-swap). The
    /// state is an aggregate over the three sessions: ANY demand ends the
    /// idle-unloaded state, and the remaining slots reload on their own
    /// first use.
    pub fn mark_loaded(&self, now: i64) {
        self.unloaded_since.store(0, ORD);
        self.loaded_since.store(now, ORD);
    }

    /// The `/system.models_state` contract: `{"state":"loaded"|"unloaded_idle",
    /// "since":<unix>}` — `unloaded_idle` is the exact string the pane's idle
    /// badge matches on.
    pub fn models_state(&self) -> serde_json::Value {
        let unloaded = self.unloaded_since.load(ORD);
        if unloaded > 0 {
            json!({ "state": "unloaded_idle", "since": unloaded })
        } else {
            json!({ "state": "loaded", "since": self.loaded_since.load(ORD) })
        }
    }
}

/// All tracker stamps are independent wall-clock scalars — relaxed is enough.
const ORD: std::sync::atomic::Ordering = std::sync::atomic::Ordering::Relaxed;

/// The machine core's identity and client census — what `/system` reports
/// under `processes`, what `engram-alpha status` renders, and the shutdown
/// trigger `POST /shutdown` fires. Constructed by the core daemon only.
pub struct CoreRuntime {
    pub pid: u32,
    pub version: String,
    /// Unix seconds the core came up.
    pub started_at: i64,
    /// The engram home dir this core serves (`~/.engram`).
    pub home: String,
    /// Idle-unload clocks + model residency state (see [`IdleTracker`]).
    pub idle: Arc<IdleTracker>,
    clients: Mutex<HashMap<String, ClientLease>>,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl CoreRuntime {
    /// Build the runtime plus the receiver the core's main loop selects on:
    /// `/shutdown` flips the watch, the core runs its orchestrated exit.
    pub fn new(home: String) -> (Arc<Self>, tokio::sync::watch::Receiver<bool>) {
        let (shutdown, rx) = tokio::sync::watch::channel(false);
        (
            Arc::new(Self {
                pid: std::process::id(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                started_at: engram_core::now(),
                home,
                idle: Arc::new(IdleTracker::new()),
                clients: Mutex::new(HashMap::new()),
                shutdown,
            }),
            rx,
        )
    }

    fn register(
        &self,
        pid: u32,
        kind: String,
        root: String,
        client: Option<String>,
    ) -> ClientLease {
        let now = engram_core::now();
        self.idle.touch_lease();
        let project = registry::load()
            .resolve_root(std::path::Path::new(&root))
            .map(|e| e.id.clone());
        let lease = ClientLease {
            lease_id: engram_core::id::new_id(),
            pid,
            kind,
            client,
            project,
            root,
            connected_at: now,
            last_seen: now,
        };
        self.clients
            .lock()
            .unwrap()
            .insert(lease.lease_id.clone(), lease.clone());
        lease
    }

    fn renew(&self, lease_id: &str, root: Option<String>, client: Option<String>) -> bool {
        match self.clients.lock().unwrap().get_mut(lease_id) {
            Some(lease) => {
                lease.last_seen = engram_core::now();
                // A roots rebind moves the row instead of minting a second
                // lease — the census (and the pane) key on lease_id.
                if let Some(root) = root.filter(|r| *r != lease.root) {
                    lease.project = registry::load()
                        .resolve_root(std::path::Path::new(&root))
                        .map(|e| e.id.clone());
                    lease.root = root;
                }
                // The client name rides renewals too: a fixed-target bridge
                // registers before its stdio client's initialize named one.
                if client.is_some() {
                    lease.client = client;
                }
                // A renewing lease is a connected client: it keeps models
                // resident through the lease clock — deliberately NOT the
                // HTTP-activity clock (pings are exempt there).
                self.idle.touch_lease();
                true
            }
            None => false,
        }
    }

    fn remove(&self, lease_id: &str) -> bool {
        self.clients.lock().unwrap().remove(lease_id).is_some()
    }

    /// Live leases, expired ones pruned on the way out (lazy expiry — no
    /// timer needed, every reader sees a fresh census).
    pub fn clients(&self) -> Vec<ClientLease> {
        let now = engram_core::now();
        let ttl = lease_ttl_secs();
        let mut clients = self.clients.lock().unwrap();
        clients.retain(|_, l| now - l.last_seen <= ttl);
        let mut out: Vec<ClientLease> = clients.values().cloned().collect();
        out.sort_by_key(|l| l.connected_at);
        out
    }
}

impl AppState {
    pub fn new(engine: Engine) -> Self {
        Self::from_hub(Arc::new(Hub::single(engine)), None)
    }

    /// Build state around a shared engine and install the change listener that
    /// turns every mutation — from this API *or* from Claude over MCP — into an
    /// SSE message.
    pub fn shared(engine: Arc<Mutex<Engine>>) -> Self {
        Self::from_hub(Arc::new(Hub::single_shared(engine)), None)
    }

    pub fn shared_with_db(engine: Arc<Mutex<Engine>>, db_path: Option<String>) -> Self {
        Self::from_hub(Arc::new(Hub::single_shared(engine)), db_path)
    }

    /// The full multi-project form: every engine the hub opens (now or later)
    /// gets a listener feeding that project's SSE channel.
    pub fn from_hub(hub: Arc<Hub>, db_path: Option<String>) -> Self {
        Self::from_hub_observed(hub, db_path, None)
    }

    /// [`AppState::from_hub`] with an optional idle tracker whose graph-write
    /// counter the change listener bumps — the librarian's dirty flag rides
    /// the same chokepoint every curated mutation (pane, MCP, imports, the
    /// librarian's own archives) already flows through.
    fn from_hub_observed(
        hub: Arc<Hub>,
        db_path: Option<String>,
        idle: Option<Arc<IdleTracker>>,
    ) -> Self {
        let events: EventMap = Arc::default();
        let ev = events.clone();
        hub.set_listener_factory(Box::new(move |project_id: &str| {
            let tx = channel(&ev, project_id);
            let idle = idle.clone();
            Box::new(move |change| {
                if let Some(idle) = &idle {
                    idle.note_graph_write();
                }
                let _ = tx.send(encode_event(&change));
            })
        }));
        Self {
            hub,
            events,
            db_path,
            started: std::time::Instant::now(),
            model_admin: None,
            skill_admin: None,
            runtime: None,
            refs_seen: Mutex::new(HashMap::new()),
        }
    }

    /// The launch project's engine, pane-stamped (see [`pane`]).
    fn engine(&self) -> MutexGuard<'_, Engine> {
        let mut guard = self.hub.current().engine.lock().unwrap();
        guard.set_audit_origin(AuditOrigin::pane());
        guard
    }

    /// Resolve the request's project scope to an engine. `all` never lands
    /// here — the two fan-out reads (search, check_claim) special-case it
    /// before resolving; everywhere else the hub's refusal explains the rule.
    fn engine_arc(&self, scope: &Scope) -> Result<Arc<Mutex<Engine>>, AppError> {
        match &scope.0 {
            None => Ok(self.hub.current().engine.clone()),
            Some(sel) => Ok(self.hub.get(sel)?),
        }
    }

    /// The repo root code_refs resolve against. Scoped requests use the
    /// scoped engine's own root (set when its store was opened); the launch
    /// project falls back to the served DB path, then cwd.
    fn repo_root(&self) -> std::path::PathBuf {
        self.db_path
            .as_deref()
            .map(std::path::Path::new)
            .and_then(|db| {
                let dir = db.parent()?;
                if dir.file_name()? != ".engram" {
                    return None;
                }
                dir.parent()
            })
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()))
    }

    fn scoped_root(&self, engine: &Arc<Mutex<Engine>>) -> std::path::PathBuf {
        engine
            .lock()
            .unwrap()
            .repo_root()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| self.repo_root())
    }
}

/// Lock a scoped engine and stamp the pane as the writer. Front-ends sharing
/// an engine re-stamp under the lock on every operation (MCP does the same).
fn pane(engine: &Arc<Mutex<Engine>>) -> MutexGuard<'_, Engine> {
    let mut guard = engine.lock().unwrap();
    guard.set_audit_origin(AuditOrigin::pane());
    guard
}

fn channel(events: &EventMap, project_id: &str) -> broadcast::Sender<String> {
    events
        .lock()
        .unwrap()
        .entry(project_id.to_string())
        .or_insert_with(|| broadcast::channel(256).0)
        .clone()
}

fn encode_event(ev: &ChangeEvent) -> String {
    let (kind, data) = match ev {
        ChangeEvent::NodeAdded(n) => ("node_added", json!(n)),
        ChangeEvent::NodeUpdated(n) => ("node_updated", json!(n)),
        ChangeEvent::NodeDeleted(id) => ("node_deleted", json!({ "id": id })),
        ChangeEvent::EdgeAdded(e) => ("edge_added", json!(e)),
        ChangeEvent::EdgeUpdated(e) => ("edge_updated", json!(e)),
        ChangeEvent::EdgeDeleted(id) => ("edge_deleted", json!({ "id": id })),
        ChangeEvent::SuspectsChanged => ("suspects_changed", json!({})),
        ChangeEvent::ConfigChanged => ("config_changed", json!({})),
    };
    json!({ "type": kind, "data": data }).to_string()
}

// ---- project scoping ----------------------------------------------------

/// The selector a `/projects/{sel}/…` URL carried, stashed by the rewrite
/// layer; absent on bare routes (= the launch project).
#[derive(Clone)]
struct ScopeSel(String);

struct Scope(Option<String>);

impl<S: Send + Sync> FromRequestParts<S> for Scope {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Scope(
            parts.extensions.get::<ScopeSel>().map(|s| s.0.clone()),
        ))
    }
}

/// Rewrite `/projects/{sel}/rest…` to `/rest…` + a [`ScopeSel`] extension,
/// so the whole graph API exists once and serves every project. Two-segment
/// paths (`/projects`, `/projects/{id}`) are registry meta ops and pass
/// through untouched. Runs *before* routing: middleware attached with
/// `Router::layer` runs after the route has matched, so this one wraps the
/// whole router through an outer `fallback_service` instead.
async fn project_scope_rewrite(mut req: Request) -> Request {
    let path = req.uri().path();
    if let Some(rest) = path.strip_prefix("/projects/")
        && let Some((sel, tail)) = rest.split_once('/')
        && !tail.is_empty()
        && !sel.is_empty()
    {
        let sel = sel.to_string();
        let new_path_q = match req.uri().query() {
            Some(q) => format!("/{tail}?{q}"),
            None => format!("/{tail}"),
        };
        if let Ok(new_uri) = new_path_q.parse() {
            req.extensions_mut().insert(ScopeSel(sel));
            *req.uri_mut() = new_uri;
        }
    }
    req
}

/// Build the full router from an already-constructed engine.
pub fn app(engine: Engine) -> Router {
    router(Arc::new(AppState::new(engine)))
}

/// Build the router around a shared engine (used by the daemon, which also
/// hands the same engine to the MCP server).
pub fn router_shared(engine: Arc<Mutex<Engine>>) -> Router {
    router(Arc::new(AppState::shared(engine)))
}

/// Like [`router_shared`], with the served DB path advertised via `/health`
/// so port-discovering clients can confirm they found the right daemon.
pub fn router_shared_with_db(engine: Arc<Mutex<Engine>>, db_path: String) -> Router {
    router(Arc::new(AppState::shared_with_db(engine, Some(db_path))))
}

/// The multi-project daemon (PLAN §7C): one router over a hub.
pub fn router_hub(hub: Arc<Hub>, db_path: Option<String>) -> Router {
    router(Arc::new(AppState::from_hub(hub, db_path)))
}

/// [`router_hub`] plus the daemon's model-selection hands (PLAN §7A).
pub fn router_hub_with_models(
    hub: Arc<Hub>,
    db_path: Option<String>,
    admin: Arc<dyn ModelAdmin>,
) -> Router {
    let mut state = AppState::from_hub(hub, db_path);
    state.model_admin = Some(admin);
    router(Arc::new(state))
}

/// [`router_hub_with_models`] plus skill-installation hands (PLAN §7D).
pub fn router_hub_with_admins(
    hub: Arc<Hub>,
    db_path: Option<String>,
    models: Arc<dyn ModelAdmin>,
    skills: Arc<dyn SkillAdmin>,
) -> Router {
    let mut state = AppState::from_hub(hub, db_path);
    state.model_admin = Some(models);
    state.skill_admin = Some(skills);
    router(Arc::new(state))
}

/// The machine core's router: [`router_hub_with_admins`] plus the process
/// census (`/clients`, `/system.processes`) and the orchestrated `/shutdown`.
pub fn router_hub_core(
    hub: Arc<Hub>,
    db_path: Option<String>,
    models: Arc<dyn ModelAdmin>,
    skills: Arc<dyn SkillAdmin>,
    runtime: Arc<CoreRuntime>,
) -> Router {
    let mut state = AppState::from_hub_observed(hub, db_path, Some(runtime.idle.clone()));
    state.model_admin = Some(models);
    state.skill_admin = Some(skills);
    state.runtime = Some(runtime);
    router(Arc::new(state))
}

fn api_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/system", get(system))
        .route("/clients", post(clients_register))
        .route("/clients/{lease_id}/ping", post(clients_ping))
        .route(
            "/clients/{lease_id}",
            axum::routing::delete(clients_unregister),
        )
        .route("/shutdown", post(shutdown_core))
        .route("/settings", get(get_settings).post(post_settings))
        .route("/models", get(models_describe).post(models_apply))
        .route("/projects", get(list_projects).post(register_project))
        .route("/projects/{id}", axum::routing::delete(unregister_project))
        .route("/fs/dirs", get(fs_dirs))
        .route("/nodes", post(create_node))
        .route(
            "/nodes/{id}",
            get(get_node).patch(patch_node).delete(delete_node),
        )
        .route("/nodes/{id}/edges", get(node_edges))
        .route("/nodes/{id}/reconfirm", post(reconfirm))
        .route("/nodes/{id}/approve", post(approve).delete(revoke_approval))
        .route("/nodes/{id}/pin", post(pin))
        .route("/nodes/{id}/traverse", get(traverse))
        .route("/edges", post(create_edge))
        .route(
            "/edges/{id}",
            axum::routing::patch(patch_edge).delete(delete_edge),
        )
        .route("/search", get(search))
        .route("/tags", get(tags))
        .route("/conflicts/suspects", get(list_suspects))
        .route("/conflicts/suspects/{id}/resolve", post(resolve_suspect))
        .route("/conflicts/agreement", get(nli_agreement))
        .route("/conflicts/scan", post(scan_conflicts))
        .route("/claims/check", post(check_claim))
        .route("/audit/conflicts", post(audit_conflicts))
        .route("/audit/duplicates", post(audit_duplicates))
        .route("/audit/answered", post(audit_answered))
        .route("/audit/promotions", post(audit_promotions))
        .route("/drift", get(drift))
        .route("/digest/scan", post(digest_scan))
        .route("/nodes/{id}/timeline", get(timeline))
        .route("/decay", post(decay))
        .route("/brief", get(brief))
        .route("/audit", get(audit))
        .route("/open", get(list_open))
        .route("/graph", get(graph))
        .route("/export", get(export))
        .route("/import", post(import))
        .route("/config", get(get_config).put(put_config))
        .route("/config/presets", get(config_presets))
        .route("/version", get(get_version).put(put_version))
        .route("/config/rename-type", post(rename_type))
        .route("/config/rename-verb", post(rename_verb))
        .route("/skills/install", post(skills_install))
        .route("/refs/match", get(refs_match))
        .route("/history", get(history_stats).delete(history_reset))
        .route("/nodes/{id}/born-in", get(node_born_in))
        .route("/history/sessions", get(history_sessions))
        .route(
            "/history/sessions/{sid}",
            get(history_session_messages).delete(history_session_delete),
        )
        .route("/audit/stale", post(audit_stale))
        .route("/events", get(sse))
        // Anything not an API route is the Vue pane (served from the embedded
        // build), so `engram-alpha serve` is a complete browser-standalone app and
        // the IDE wrappers just point a webview at this one URL.
        .fallback(static_pane)
        .layer(local_cors())
        .with_state(state)
}

/// CORS for a localhost-only daemon (SECURITY.md hardening): same-machine
/// browser pages on loopback and the IDE webviews the pane embeds under are
/// allowed; a random website in the user's browser is not — the previously
/// permissive layer let any page read the whole graph API. Requests without
/// an Origin header (curl, the IDE's same-origin JCEF view, MCP bridges)
/// never hit CORS at all.
fn local_cors() -> CorsLayer {
    use axum::http::HeaderValue;
    CorsLayer::new()
        .allow_origin(tower_http::cors::AllowOrigin::predicate(
            |origin: &HeaderValue, _| {
                let Ok(origin) = origin.to_str() else {
                    return false;
                };
                let localhost = origin.strip_prefix("http://").is_some_and(|rest| {
                    rest.starts_with("127.0.0.1:")
                        || rest == "127.0.0.1"
                        || rest.starts_with("localhost:")
                        || rest == "localhost"
                });
                // VS Code / VSCodium / Cursor / Windsurf webviews fetch with
                // their per-webview pseudo-origin.
                localhost || origin.starts_with("vscode-webview://")
            },
        ))
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any)
}

pub fn router(state: Arc<AppState>) -> Router {
    // The scope rewrite must see the URI before any route matches, so it
    // wraps the whole API router: the outer router routes nothing itself.
    Router::new()
        .fallback_service(api_router(state))
        .layer(middleware::map_request(project_scope_rewrite))
}

/// The production frontend, embedded at build time (read from disk in debug).
#[derive(RustEmbed)]
#[folder = "../../frontend/dist"]
struct Pane;

/// Serve an embedded asset by path; fall back to `index.html` so client-side
/// routes (and a bare `/`) resolve to the single-page app.
async fn static_pane(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(file) = Pane::get(path) {
        let mime = file.metadata.mimetype().to_owned();
        return (
            [(axum::http::header::CONTENT_TYPE, mime)],
            file.data.into_owned(),
        )
            .into_response();
    }
    match Pane::get("index.html") {
        Some(index) => (
            [(axum::http::header::CONTENT_TYPE, "text/html".to_owned())],
            index.data.into_owned(),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            "Engram pane was not embedded in this build (run `bun run build` in frontend/).",
        )
            .into_response(),
    }
}

// ---- responses ----------------------------------------------------------

#[derive(Serialize)]
struct GraphResponse {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

#[derive(Serialize)]
struct EdgesResponse {
    out: Vec<Edge>,
    #[serde(rename = "in")]
    incoming: Vec<Edge>,
}

async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "db": state.db_path,
    }))
}

/// The current model selection + presets, or `{"available": false}` when this
/// process has no model-selection hands (library/test embeddings).
async fn models_describe(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    match &state.model_admin {
        Some(admin) => Json(admin.describe()),
        None => Json(json!({ "available": false })),
    }
}

/// Apply a model selection: `{"role", "preset"}` or `{"role", "custom"}`.
/// Blocking by design — the response arrives after the download, the load,
/// the live swap, and (for embeddings) the full re-embed have all happened.
async fn models_apply(
    State(state): State<Arc<AppState>>,
    Json(request): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let Some(admin) = state.model_admin.clone() else {
        return Err(AppError::Core(Error::Project(
            "model selection needs the daemon (engram-alpha serve)".into(),
        )));
    };
    let result = tokio::task::spawn_blocking(move || admin.apply(request))
        .await
        .map_err(|e| AppError::Core(Error::Io(e.to_string())))??;
    Ok(Json(result))
}

/// The pane's System info (Settings → System): the doctor's daemon-side facts
/// as structured JSON — binary version, store health, model cache, and which
/// assistants are wired to this repo. Everything is best-effort: a partial
/// report beats a 500 on a diagnostics screen. Always the launch project —
/// per-project store facts are one `/projects/{id}/system`-shaped question
/// the registry view doesn't need yet.
async fn system(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let repo = state.repo_root();
    let wiring = engram_core::harness::wiring(&repo);
    let model_cached = engram_core::harness::home_file(".cache/engram").is_some_and(|dir| {
        std::fs::read_dir(&dir).is_ok_and(|mut entries| entries.next().is_some())
    });
    // Active model identities come from the machine-level selection
    // (~/.engram/models.json); a role without a selection is the default,
    // whose path keeps honoring its ENGRAM_*_DIR override.
    let cortex_cfg = engram_core::cortex::load();
    let dir_str =
        |d: Option<std::path::PathBuf>| d.map(|p| p.display().to_string()).unwrap_or_default();
    let role_info = |role: engram_core::cortex::Role,
                     default_dir: Option<std::path::PathBuf>|
     -> (String, String) {
        let spec = cortex_cfg.effective(role);
        let dir = if cortex_cfg.get(role).is_none() {
            default_dir
        } else {
            engram_core::cortex::cache_dir(&spec.name)
        };
        (spec.name, dir_str(dir))
    };
    let (embed_name, embed_dir) = role_info(
        engram_core::cortex::Role::Embedding,
        engram_core::rag::model_dir(),
    );
    let (rerank_name, rerank_dir) = role_info(
        engram_core::cortex::Role::Reranker,
        engram_core::rag::reranker_model_dir(),
    );
    let (nli_name, nli_dir) = role_info(
        engram_core::cortex::Role::Nli,
        engram_core::nli::nli_model_dir(),
    );
    let db_size = state
        .db_path
        .as_deref()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len());

    let engine = state.engine();
    let store = engine.store();
    let stats = store.stats().ok();
    let health = store.health().ok();
    let embed_version = store.embed_version().unwrap_or(0);

    // The process census (core daemons only): who the core is and which
    // clients hold a live lease. `models_state` reports ONNX residency —
    // `loaded` or `unloaded_idle` (idle-unload) — with `since` marking when
    // the current residency began.
    let processes = state.runtime.as_ref().map(|rt| {
        (
            json!({
                "core": {
                    "pid": rt.pid,
                    "version": rt.version,
                    "started_at": rt.started_at,
                    "home": rt.home,
                },
                "clients": rt.clients(),
                // Live MCP session bindings (0.8.9 set_project): where each
                // session IS, which after an in-session rebind is not what
                // its bridge's launch lease says.
                "sessions": state.hub.sessions(),
            }),
            rt.idle.models_state(),
        )
    });

    let mut out = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "daemon": {
            "pid": std::process::id(),
            "uptime_secs": state.started.elapsed().as_secs(),
            "repo_root": repo.display().to_string(),
        },
        "store": {
            "db": state.db_path,
            "size_bytes": db_size,
            "backend": stats.as_ref().map(|s| s.backend).unwrap_or("unknown"),
            "nodes": stats.as_ref().map(|s| s.nodes).unwrap_or(-1),
            "edges": stats.as_ref().map(|s| s.edges).unwrap_or(-1),
            "embedded": stats.as_ref().map(|s| s.embedded).unwrap_or(-1),
            "journal_mode": health.as_ref().and_then(|h| h.journal_mode.clone()).unwrap_or_default(),
            "integrity_ok": health.as_ref().is_some_and(|h| h.integrity_ok),
            "embed_composition": embed_version,
            "embed_composition_current": embed_version >= engram_core::EMBED_COMPOSITION,
        },
        "model_cached": model_cached,
        "reranker": engine.has_reranker(),
        "nli": engine.has_nli(),
        // The local cortex (PLAN §7A), one row per model with its on-disk home.
        "models": [
            {
                "name": embed_name,
                "role": format!(
                    "embeddings — recall ({}-dim vectors, hybrid search)",
                    engine.embed_model_id().dim
                ),
                "path": embed_dir,
                "active": !engine.embeddings_are_fake(),
            },
            {
                "name": rerank_name,
                "role": "reranker — search precision (cross-encoder)",
                "path": rerank_dir,
                "active": engine.has_reranker(),
            },
            {
                "name": nli_name,
                "role": "NLI — logic (conflict hints, claim checks, Checkup sweeps)",
                "path": nli_dir,
                "active": engine.has_nli(),
            },
        ],
        "model_selection": state.model_admin.is_some(),
        "wiring": wiring,
    });
    if let Some((processes, models_state)) = processes {
        out["processes"] = processes;
        out["models_state"] = models_state;
    }
    Json(out)
}

// ---- client leases + shutdown (core daemons only) ------------------------

#[derive(Deserialize)]
struct RegisterClientBody {
    pid: u32,
    kind: String,
    /// Absolute project root the client is bound to.
    root: String,
    /// The MCP client's name (`clientInfo.name` from the stdio initialize).
    /// Optional: pre-0.8.8 bridges (and registrations racing the handshake)
    /// omit it.
    #[serde(default)]
    client: Option<String>,
}

fn core_runtime(state: &AppState) -> Result<&Arc<CoreRuntime>, AppError> {
    state.runtime.as_ref().ok_or(AppError::NotFound)
}

/// A client (MCP bridge) announces itself: `{pid, kind, root}` → a lease it
/// must renew (`/clients/{lease}/ping`) to stay in the census.
async fn clients_register(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterClientBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let lease = core_runtime(&state)?.register(body.pid, body.kind, body.root, body.client);
    Ok(Json(json!({ "lease_id": lease.lease_id })))
}

#[derive(Deserialize)]
struct PingClientBody {
    /// The bridge's current project root — carried on every renewal so a
    /// roots rebind moves the census row (same lease_id) instead of adding
    /// one. Optional: pre-0.8.8 bridges ping with no body.
    root: Option<String>,
    /// The MCP client's name, once the bridge learned it from the stdio
    /// initialize (a fixed-target bridge registers before that handshake).
    #[serde(default)]
    client: Option<String>,
}

/// Renew a lease. 404 = the lease expired (or the core restarted) — the
/// client should register again.
async fn clients_ping(
    State(state): State<Arc<AppState>>,
    Path(lease_id): Path<String>,
    body: Option<Json<PingClientBody>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (root, client) = body.map(|Json(b)| (b.root, b.client)).unwrap_or_default();
    if core_runtime(&state)?.renew(&lease_id, root, client) {
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(AppError::NotFound)
    }
}

/// Clean exit: the client withdraws its lease instead of letting it lapse.
async fn clients_unregister(
    State(state): State<Arc<AppState>>,
    Path(lease_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let removed = core_runtime(&state)?.remove(&lease_id);
    Ok(Json(json!({ "removed": removed })))
}

/// The orchestrated stop (loopback-only like the whole API): reply with what
/// is about to stop, then flip the core's shutdown watch — the core's main
/// loop runs the exit sequence (close sessions, drop engines, remove daemon
/// files, exit 0). The trigger is delayed a beat so this reply reaches the
/// stopper before the process starts dying.
async fn shutdown_core(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let rt = core_runtime(&state)?;
    let clients = rt.clients().len();
    let tx = rt.shutdown.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let _ = tx.send(true);
    });
    Ok(Json(
        json!({ "stopping": true, "pid": rt.pid, "clients": clients }),
    ))
}

// ---- machine-level settings (core daemons only, like the census) ----------

/// One settings reply: the stored selector plus, when it resolves, the
/// project's current name/root so the pane can render it without a join.
fn settings_view(s: &engram_core::settings::Settings) -> serde_json::Value {
    let resolved = s
        .default_agent_project
        .as_deref()
        .and_then(|sel| registry::load().resolve(sel).cloned());
    json!({
        "default_agent_project": s.default_agent_project,
        "default_agent_project_name": resolved.as_ref().map(|e| e.name.clone()),
        "default_agent_project_root": resolved.map(|e| e.root),
    })
}

/// The machine-level settings (`~/.engram/settings.json`). Core daemons only
/// — a non-core daemon 404s, which is also how an older pane knows to hide
/// the control.
async fn get_settings(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, AppError> {
    core_runtime(&state)?;
    Ok(Json(settings_view(&engram_core::settings::load())))
}

#[derive(Deserialize)]
struct SettingsBody {
    /// Project id or name; `null`, `""`, or `"home"` clears the setting
    /// (sessions fall to the home graph, today's behavior).
    default_agent_project: Option<String>,
}

/// Write the machine-level settings. The project must exist (registry id or
/// name, or "home") — anything else is a 400 and the file is untouched.
/// Applies to FUTURE bindings only: already-connected sessions keep the
/// project they bound.
async fn post_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SettingsBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    core_runtime(&state)?;
    let stored = match body.default_agent_project.as_deref() {
        None | Some("") | Some(registry::HOME_PROJECT) => None,
        Some(sel) => {
            let entry = registry::load().resolve(sel).cloned().ok_or_else(|| {
                AppError::Core(Error::Project(format!(
                    "unknown project '{sel}' — register it first (engram-alpha serve), or pass \"home\""
                )))
            })?;
            // Stored as the stable id: renames of the human slug can't strand
            // the setting, and the bridge's registry lookup takes either.
            Some(entry.id)
        }
    };
    let mut settings = engram_core::settings::load();
    settings.default_agent_project = stored;
    engram_core::settings::save(&settings)?;
    Ok(Json(settings_view(&settings)))
}

// ---- project registry (PLAN §7C) ----------------------------------------

/// Every project the hub can reach: current, home, registry — the pane's
/// switcher and the Settings registry view both read this.
async fn list_projects(State(state): State<Arc<AppState>>) -> Json<Vec<ProjectInfo>> {
    Json(state.hub.projects())
}

#[derive(Deserialize)]
struct RegisterBody {
    /// Absolute path of the repo to register (its `.engram/graph.db` is
    /// created lazily on first access).
    path: String,
}

async fn register_project(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterBody>,
) -> Result<Json<registry::ProjectEntry>, AppError> {
    let root = std::path::PathBuf::from(&body.path);
    if !root.is_dir() {
        return Err(AppError::Core(Error::Project(format!(
            "not a directory: {}",
            body.path
        ))));
    }
    let db = root.join(".engram/graph.db");
    let entry = registry::register(&root, &db)?;
    // The daemon this pane talks to serves the list — refresh is one GET away.
    let _ = state;
    Ok(Json(entry))
}

/// Withdraw a project from the registry — awareness only; its data stays
/// where it lives. The current project and the home graph are not entries.
async fn unregister_project(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    if id == registry::HOME_PROJECT
        || id == state.hub.current().id
        || id == state.hub.current().name
    {
        return Err(AppError::Core(Error::Project(
            "the current project and the home graph are not registry entries".into(),
        )));
    }
    let removed = registry::unregister(&id)?;
    if !removed {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct FsDirsParams {
    /// Absolute directory to list; omitted = the user's home directory.
    path: Option<String>,
}

/// Directory listing for the pane's folder picker (PLAN §7C add-by-path):
/// a browser can never reveal an absolute filesystem path, so the daemon —
/// which owns the filesystem anyway — serves the navigation. Directories
/// only, dot-dirs hidden, unreadable entries skipped; each row says whether
/// it already carries an `.engram` graph or is a git repo.
async fn fs_dirs(Query(p): Query<FsDirsParams>) -> Result<Json<serde_json::Value>, AppError> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(std::path::PathBuf::from);
    let start = p
        .path
        .map(std::path::PathBuf::from)
        .or_else(|| home.clone())
        .ok_or_else(|| AppError::Core(Error::Project("no home directory".into())))?;
    let listing = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, Error> {
        let path = start
            .canonicalize()
            .map_err(|e| Error::Project(format!("not a readable directory: {e}")))?;
        if !path.is_dir() {
            return Err(Error::Project(format!(
                "not a directory: {}",
                path.display()
            )));
        }
        let mut dirs = Vec::new();
        for entry in std::fs::read_dir(&path)
            .map_err(|e| Error::Project(format!("can't list {}: {e}", path.display())))?
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || !entry.path().is_dir() {
                continue;
            }
            let p = entry.path();
            dirs.push(json!({
                "name": name,
                "path": p.display().to_string(),
                "engram": p.join(".engram/graph.db").is_file(),
                "git": p.join(".git").exists(),
            }));
        }
        dirs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Ok(json!({
            "path": path.display().to_string(),
            "parent": path.parent().map(|p| p.display().to_string()),
            "home": home.map(|h| h.display().to_string()),
            "dirs": dirs,
        }))
    })
    .await
    .expect("directory listing never panics — every entry error is a skip")?;
    Ok(Json(listing))
}

// ---- node handlers ------------------------------------------------------

async fn create_node(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(input): Json<NewNode>,
) -> Result<Json<Node>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let node = pane(&engine).add_node(input)?;
    Ok(Json(node))
}

async fn get_node(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<Json<Node>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let node = pane(&engine).get_node(&id)?;
    node.map(Json).ok_or(AppError::NotFound)
}

async fn patch_node(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
    Json(patch): Json<NodePatch>,
) -> Result<Json<Node>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let node = {
        let engine = pane(&engine);
        if engine.get_node(&id)?.is_none() {
            return Err(AppError::NotFound);
        }
        engine.update_node(&id, patch)?
    };
    Ok(Json(node))
}

async fn delete_node(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let engine = state.engine_arc(&scope)?;
    let removed = pane(&engine).delete_node(&id)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

async fn node_edges(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<Json<EdgesResponse>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let engine = pane(&engine);
    if engine.get_node(&id)?.is_none() {
        return Err(AppError::NotFound);
    }
    Ok(Json(EdgesResponse {
        out: engine.edges_out(&id)?,
        incoming: engine.edges_in(&id)?,
    }))
}

async fn traverse(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
    Query(p): Query<TraverseParams>,
) -> Result<Json<GraphResponse>, AppError> {
    let edge_types = parse_edge_types(p.edge_types.as_deref())?;
    let depth = p.depth.unwrap_or(2);
    let engine = state.engine_arc(&scope)?;
    let (nodes, edges) = engine.lock().unwrap().traverse(&id, &edge_types, depth)?;
    Ok(Json(GraphResponse { nodes, edges }))
}

// ---- edge handler -------------------------------------------------------

async fn create_edge(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(input): Json<NewEdge>,
) -> Result<Json<Edge>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let edge = {
        let engine = pane(&engine);
        // Surface dangling endpoints as 404 rather than an opaque FK failure.
        if engine.get_node(&input.from_id)?.is_none() || engine.get_node(&input.to_id)?.is_none() {
            return Err(AppError::NotFound);
        }
        engine.add_edge(input)?
    };
    Ok(Json(edge))
}

async fn patch_edge(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
    Json(patch): Json<EdgePatch>,
) -> Result<Json<Edge>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let edge = pane(&engine).update_edge(&id, patch)?;
    Ok(Json(edge))
}

async fn delete_edge(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let engine = state.engine_arc(&scope)?;
    let removed = pane(&engine).delete_edge(&id)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

// ---- search / read handlers --------------------------------------------

async fn search(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<SearchParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let types = parse_node_types(p.types.as_deref())?;
    let limit = p.limit.unwrap_or(8);
    // `all` is the one read fan-out: current project at full weight, every
    // sibling + home under the locality prior, provenance on foreign hits.
    if scope.0.as_deref() == Some(registry::ALL_PROJECTS) {
        let (hits, skipped) = state.hub.search_all(&p.q, &types, limit)?;
        return Ok(Json(json!({ "hits": hits, "skipped": skipped })));
    }
    let engine = state.engine_arc(&scope)?;
    let filter = engine.lock().unwrap().time_filter(
        p.after.as_deref(),
        p.before.as_deref(),
        p.during_version.as_deref(),
        p.order.as_deref(),
    )?;
    if p.scope.as_deref() == Some("history") {
        let hits = engine
            .lock()
            .unwrap()
            .search_history_filtered(&p.q, limit, &filter)?;
        return Ok(Json(json!(hits)));
    }
    let hits = pane(&engine).search_filtered(&p.q, &types, limit, &filter)?;
    Ok(Json(json!(hits)))
}

/// Tags in use on current nodes, freshest first — feeds the pane's tag
/// dropdown and filter chips (PLAN §10 tags).
async fn tags(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<TagsParams>,
) -> Result<Json<Vec<TagStat>>, AppError> {
    let limit = p.limit.unwrap_or(200);
    let engine = state.engine_arc(&scope)?;
    let tags = pane(&engine).tags(limit)?;
    Ok(Json(tags))
}

// ---- conflict scan + decay (PLAN §7 / §6B) --------------------------------

async fn list_suspects(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<Vec<SuspectView>>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let suspects = pane(&engine).suspects()?;
    Ok(Json(suspects))
}

/// The Checkup panel's NLI scoreboard: how often the local model's suspect
/// hint agreed with the judge's actual verdict, over the judged history.
async fn nli_agreement(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<NliAgreement>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let report = pane(&engine).nli_agreement()?;
    Ok(Json(report))
}

/// Verified code refs (PLAN §10): nodes whose path-shaped refs no longer
/// exist under the scoped project's root.
async fn drift(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<Vec<Drift>>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let root = state.scoped_root(&engine);
    let drifted = pane(&engine).scan_code_refs(&root)?;
    Ok(Json(drifted))
}

/// Digestion tier 1 (PLAN §7B): the gitignore-aware FIXME/TODO scan of the
/// working tree. Candidates only — the digest skill judges them and writes
/// through the normal node path. Runs outside the engine lock: it never
/// touches the store. Deliberately HTTP-only, no MCP tool.
async fn digest_scan(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<engram_core::digest::DigestScan>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let root = state.scoped_root(&engine);
    // The walk is filesystem-bound; keep it off the async workers.
    Ok(Json(
        tokio::task::spawn_blocking(move || engram_core::digest::scan(&root))
            .await
            .expect("digest scan never panics — every file error is a skip"),
    ))
}

/// Timeline (PLAN §10): the node's `replaces` chain, oldest first.
async fn timeline(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<Json<Vec<TimelineEntry>>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let chain = pane(&engine).timeline(&id)?;
    Ok(Json(chain))
}

#[derive(Deserialize)]
struct CheckClaimBody {
    text: String,
    #[serde(default)]
    limit: Option<usize>,
}

/// Verify a claim against the canon (PLAN §7A): supports / contradicts /
/// silent, each with the judging node. Requires the local NLI model.
/// Scope `all` judges across every reachable graph with provenance.
async fn check_claim(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(body): Json<CheckClaimBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let limit = body.limit.unwrap_or(8);
    if scope.0.as_deref() == Some(registry::ALL_PROJECTS) {
        let (report, skipped) = state.hub.check_claim_all(&body.text, limit)?;
        let mut out = json!(report);
        out["skipped"] = json!(skipped);
        return Ok(Json(out));
    }
    let engine = state.engine_arc(&scope)?;
    let report: ClaimReport = pane(&engine).check_claim(&body.text, limit)?;
    Ok(Json(json!(report)))
}

/// Audit-panel sweep: deep conflict pass (lower similarity floor, NLI-gated).
async fn audit_conflicts(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<AuditSweep>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let sweep = pane(&engine).audit_conflicts()?;
    Ok(Json(sweep))
}

/// Audit-panel sweep: mutual-entailment duplicates.
async fn audit_duplicates(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<AuditSweep>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let sweep = pane(&engine).audit_duplicates()?;
    Ok(Json(sweep))
}

/// Audit-panel check: open Problems/Intents that an existing node may answer.
async fn audit_answered(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<Vec<AnsweredHint>>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let hints = pane(&engine).audit_answered()?;
    Ok(Json(hints))
}

/// Promotion nominations (PLAN §7C): current-project Principles/Cautions
/// recurring in other projects, not yet represented in the home graph.
/// Read-only — the pane promotes via `POST /projects/home/nodes`.
async fn audit_promotions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (candidates, skipped) = state.hub.promotion_candidates()?;
    Ok(Json(
        json!({ "candidates": candidates, "skipped": skipped }),
    ))
}

/// Run the local candidate sweep on demand (the pane's "Scan now").
async fn scan_conflicts(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let added = pane(&engine).scan_conflicts()?;
    Ok(Json(json!({ "added": added })))
}

#[derive(Deserialize)]
struct ResolveBody {
    verdict: SuspectVerdict,
}

/// Judge a suspected pair from the pane — a user action, so edges it creates
/// are user-sourced.
async fn resolve_suspect(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
    Json(body): Json<ResolveBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let edge = pane(&engine).resolve_suspect(&id, body.verdict, engram_core::Source::User)?;
    Ok(Json(json!({ "edge": edge })))
}

#[derive(Deserialize)]
struct DecayParams {
    ttl_days: Option<i64>,
    dry_run: Option<bool>,
}

/// The decay pass (PLAN §6B). `dry_run=true` previews what would archive.
/// History-layer status for the pane's settings section: whether the layer
/// is open, and what it holds. Cheap enough to poll.
async fn history_stats(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let engine = engine.lock().unwrap();
    let cfg = engine.config();
    let stats = engine.history_stats();
    Ok(Json(json!({
        "enabled": cfg.history.enabled,
        "open": engine.history_open(),
        "search_fallthrough": cfg.history.search_fallthrough,
        "stats": stats,
    })))
}

/// Wholesale history delete — user-only, like curated hard delete. Wipes
/// `history.tepin` (reopening fresh when the layer stays enabled) and bumps
/// the hub's epoch so the running harvester forgets its cursors.
async fn history_reset(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    {
        let mut engine = engine.lock().unwrap();
        engine.set_audit_origin(engram_core::AuditOrigin::pane());
        engine.reset_history()?;
    }
    state.hub.bump_history_epoch();
    Ok(Json(json!({ "reset": true })))
}

/// The curated node's birth exchange, when born-in provenance exists — the
/// pane's "history" chip.
async fn node_born_in(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let born = engine.lock().unwrap().born_in_of(&id);
    Ok(Json(json!({ "born_in": born })))
}

async fn history_sessions(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<WindowParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let window =
        engram_core::timespec::window(p.after.as_deref(), p.before.as_deref(), engram_core::now())?;
    let sessions = engine.lock().unwrap().list_history_sessions_in(window)?;
    Ok(Json(json!({ "sessions": sessions })))
}

async fn history_session_messages(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(sid): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let messages = engine.lock().unwrap().history_messages(&sid)?;
    Ok(Json(json!({ "session": sid, "messages": messages })))
}

/// Per-session hard delete — pane-only, like every hard delete.
async fn history_session_delete(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(sid): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let removed = {
        let mut engine = engine.lock().unwrap();
        engine.set_audit_origin(AuditOrigin::pane());
        // No epoch bump: the deletion excludes the transcript path itself,
        // and a cursor wipe would only re-scan what's now excluded anyway.
        engine.delete_history_session(&sid)?
    };
    Ok(Json(json!({ "removed": removed })))
}

async fn decay(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<DecayParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let ids = {
        let engine = engine.lock().unwrap();
        let ttl = p
            .ttl_days
            .unwrap_or_else(|| engine.config().policy.decay_ttl_days);
        engine.decay(ttl, p.dry_run.unwrap_or(false))?
    };
    Ok(Json(json!({ "archived": ids.len(), "ids": ids })))
}

/// The session-start digest, as `text/markdown` (PLAN §6A retrieval trigger).
/// Three shapes, and the difference is which project the reader IS:
/// `/projects/{sel}/brief` is the pane's view of that graph alone;
/// `/brief?project={name|id|dir}` is the brief a session bound there
/// receives — that project plus the home-graph section and the roster — and
/// is what the SessionStart hook injects, since a hook knows its folder and
/// the machine core's own launch graph is nobody's project; plain `/brief`
/// is the same for the launch project.
async fn brief(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<BriefParams>,
) -> Result<Response, AppError> {
    // Resolve the briefed engine first (so the budget is the briefed graph's
    // own), then branch only on hub-vs-pane rendering.
    let bound = p.project.as_deref().filter(|_| scope.0.is_none());
    let engine = match bound {
        Some(sel) => state.hub.get(sel)?,
        None => state.engine_arc(&scope)?,
    };
    let max_chars = engine.lock().unwrap().brief_chars(p.max_chars);
    let text = match &scope.0 {
        None => state.hub.brief_for(bound, max_chars)?,
        Some(_) => pane(&engine).brief(max_chars)?,
    };
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/markdown; charset=utf-8",
        )],
        text,
    )
        .into_response())
}

/// One page of the audit journal, newest first (PLAN §10). Keyset pagination:
/// pass the last entry's `seq` as `before` for the next page; `entity_id`
/// narrows to one node/edge's history.
async fn audit(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<AuditParams>,
) -> Result<Json<AuditPage>, AppError> {
    let limit = p.limit.unwrap_or(50).min(200);
    let engine = state.engine_arc(&scope)?;
    let page = pane(&engine).audit_log(p.before, p.entity_id.as_deref(), limit)?;
    Ok(Json(page))
}

async fn list_open(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<TypesParam>,
) -> Result<Json<Vec<Node>>, AppError> {
    let types = parse_node_types(p.types.as_deref())?;
    let engine = state.engine_arc(&scope)?;
    let nodes = pane(&engine).list_open(&types)?;
    Ok(Json(nodes))
}

async fn graph(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<GraphResponse>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let (nodes, edges) = pane(&engine).graph()?;
    Ok(Json(GraphResponse { nodes, edges }))
}

async fn reconfirm(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<Json<Node>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let engine = pane(&engine);
    if engine.get_node(&id)?.is_none() {
        return Err(AppError::NotFound);
    }
    let node = engine.reconfirm(&id)?;
    Ok(Json(node))
}

async fn approve(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<Json<Node>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let engine = pane(&engine);
    if engine.get_node(&id)?.is_none() {
        return Err(AppError::NotFound);
    }
    let node = engine.approve(&id)?;
    Ok(Json(node))
}

/// Withdraw an approval (and any pin) — trust falls back to the
/// confirmed/created anchor.
async fn revoke_approval(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<Json<Node>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let engine = pane(&engine);
    if engine.get_node(&id)?.is_none() {
        return Err(AppError::NotFound);
    }
    let node = engine.revoke_approval(&id)?;
    Ok(Json(node))
}

#[derive(Deserialize)]
struct PinBody {
    /// Constant trust in 0..=1 (pin = 1.0); null clears the pin.
    value: Option<f64>,
}

/// Set or clear the constant-trust pin (trust v2). User-only, like the
/// hard delete — the MCP server deliberately exposes no counterpart.
async fn pin(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
    Json(body): Json<PinBody>,
) -> Result<Json<Node>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let engine = pane(&engine);
    if engine.get_node(&id)?.is_none() {
        return Err(AppError::NotFound);
    }
    let node = engine.set_trust_override(&id, body.value)?;
    Ok(Json(node))
}

async fn export(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<ExportGraph>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let graph = pane(&engine).export()?;
    Ok(Json(graph))
}

async fn import(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(graph): Json<ExportGraph>,
) -> Result<Json<ImportSummary>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let summary = pane(&engine).import(graph)?;
    Ok(Json(summary))
}

async fn get_config(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<engram_core::GraphConfig>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let cfg = pane(&engine).graph_config();
    Ok(Json(cfg))
}

async fn put_config(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(cfg): Json<engram_core::GraphConfig>,
) -> Result<Json<engram_core::GraphConfig>, AppError> {
    let engine = state.engine_arc(&scope)?;
    pane(&engine).set_graph_config(&cfg)?;
    Ok(Json(cfg))
}

#[derive(Deserialize)]
struct PutVersionParams {
    version: Option<String>,
}

/// The graph's current working version (version tracking, 0.7.0).
async fn get_version(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let (enabled, current) = {
        let engine = pane(&engine);
        (
            engine.graph_config().versioning.enabled,
            engine.current_version()?,
        )
    };
    Ok(Json(json!({ "enabled": enabled, "current": current })))
}

async fn put_version(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(p): Json<PutVersionParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let previous = pane(&engine).set_current_version(p.version.as_deref())?;
    Ok(Json(json!({ "previous": previous, "current": p.version })))
}

/// The curated ontology templates (PLAN §7D stage 4) — machine-level data,
/// same shelf for every project; applying one is a plain PUT /config.
async fn config_presets() -> Json<Vec<engram_core::config::Preset>> {
    Json(engram_core::config::presets())
}

#[derive(Deserialize)]
struct RenameParams {
    from: String,
    to: String,
}

#[derive(Deserialize)]
struct RefsMatchParams {
    /// Repo-relative file path the assistant just read or edited.
    path: String,
    /// Acting session — the server deduplicates per session so the same
    /// node is never re-injected on every read (ambient value, not noise).
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// The file-read match hook's lookup (PLAN §10 ambient hooks): nodes whose
/// code_refs cover the path, as ready-to-inject markdown. Noise control is
/// server-side — non-stale only, trust-ordered, capped, and per-session
/// deduplicated. An empty body means "nothing new to say".
async fn refs_match(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<RefsMatchParams>,
) -> Result<Response, AppError> {
    const INJECT_CAP: usize = 3;
    let limit = p.limit.unwrap_or(INJECT_CAP).clamp(1, 10);
    let engine = state.engine_arc(&scope)?;
    let hits = pane(&engine).match_code_refs(&p.path, limit * 4)?;

    let fresh: Vec<_> = match &p.session {
        None => hits.into_iter().take(limit).collect(),
        Some(session) => {
            let mut seen = state.refs_seen.lock().unwrap();
            // Bounded memory: sessions come and go; keep the map small.
            if seen.len() > 64 {
                seen.clear();
            }
            let set = seen.entry(session.clone()).or_default();
            hits.into_iter()
                .filter(|n| set.insert(n.id.clone()))
                .take(limit)
                .collect()
        }
    };
    let mut out = String::new();
    if !fresh.is_empty() {
        out.push_str(&format!(
            "Engram memory attached to {} — read before relying on or changing this code:
",
            p.path
        ));
        for n in &fresh {
            out.push_str(&engram_core::brief_line(n));
            out.push('\n');
        }
    }
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/markdown; charset=utf-8",
        )],
        out,
    )
        .into_response())
}

/// "Triage stale notes" (Checkup): what the live canon suggests doing with
/// each stale node — reconfirm / contradicted / isolated.
async fn audit_stale(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<Vec<engram_core::StaleTriage>>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let hints = pane(&engine).audit_stale_triage()?;
    Ok(Json(hints))
}

#[derive(Deserialize)]
struct SkillInstallParams {
    #[serde(default)]
    variant: Option<String>,
}

/// (Re)install the assistant capture skill into the scoped project's repo —
/// generated from the graph's ontology when it is customized, the canonical
/// variant text when it runs the shipped set (PLAN §7D teaching surface).
async fn skills_install(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(p): Json<SkillInstallParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let Some(admin) = state.skill_admin.clone() else {
        return Err(AppError::Core(engram_core::Error::Config(
            "this daemon has no skill-installation hands".into(),
        )));
    };
    let engine = state.engine_arc(&scope)?;
    let (root, cfg) = {
        let engine = engine.lock().unwrap();
        let root = engine.repo_root().map(std::path::Path::to_path_buf);
        (root, engine.graph_config())
    };
    let Some(root) = root else {
        return Err(AppError::Core(engram_core::Error::Config(
            "this graph has no repository — skills install into a project working tree".into(),
        )));
    };
    let variant = p.variant.as_deref().unwrap_or("relaxed");
    Ok(Json(admin.install(&root, &cfg, variant)?))
}

/// Rename a node type and bulk-retype its stored nodes — the ontology
/// migration gesture; a plain PUT can't do this (it refuses to strand rows).
async fn rename_type(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(p): Json<RenameParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let renamed = pane(&engine).rename_type(&p.from, &p.to)?;
    Ok(Json(json!({ "renamed": renamed })))
}

/// Rename an edge verb and bulk-retype its stored edges.
async fn rename_verb(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(p): Json<RenameParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let renamed = pane(&engine).rename_verb(&p.from, &p.to)?;
    Ok(Json(json!({ "renamed": renamed })))
}

async fn sse(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, AppError> {
    let project_id = match &scope.0 {
        None => state.hub.current().id.clone(),
        Some(sel) => {
            // Opening the engine installs its listener, so the channel is
            // live before the first subscriber attaches.
            state.hub.get(sel)?;
            state.hub.resolve_id(sel)?
        }
    };
    let stream = BroadcastStream::new(channel(&state.events, &project_id).subscribe())
        .filter_map(|msg| msg.ok().map(|s| Ok(Event::default().data(s))));
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// ---- query params -------------------------------------------------------

#[derive(Deserialize)]
struct SearchParams {
    q: String,
    limit: Option<usize>,
    types: Option<String>,
    /// "history" = search the recorded-session layer instead of curated
    /// memory (the pane's history view routes its search box here).
    scope: Option<String>,
    /// The temporal grammar (0.8.7), same shapes the MCP surface takes: a day,
    /// an ISO instant, or a relative expression the daemon resolves.
    after: Option<String>,
    before: Option<String>,
    during_version: Option<String>,
    /// "relevance" (default) | "chronological" | "recent".
    order: Option<String>,
}

/// A bare time window for the browsing endpoints (0.8.7) — same grammar as
/// search's `after`/`before`.
#[derive(Deserialize)]
struct WindowParams {
    after: Option<String>,
    before: Option<String>,
}

#[derive(Deserialize)]
struct TypesParam {
    types: Option<String>,
}

#[derive(Deserialize)]
struct TraverseParams {
    edge_types: Option<String>,
    depth: Option<usize>,
}

#[derive(Deserialize)]
struct BriefParams {
    max_chars: Option<usize>,
    /// Brief AS this project — a name, an id, or the project's directory
    /// (any path inside it). The session-brief hook holds a folder, not a
    /// name, and the machine core's launch graph is nobody's project.
    project: Option<String>,
}

#[derive(Deserialize)]
struct TagsParams {
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct AuditParams {
    limit: Option<usize>,
    before: Option<i64>,
    entity_id: Option<String>,
}

fn parse_node_types(s: Option<&str>) -> Result<Vec<NodeType>, AppError> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(vec![]),
        Some(s) => s
            .split(',')
            .map(|t| NodeType::parse(t.trim()))
            .collect::<engram_core::Result<_>>()
            .map_err(AppError::from),
    }
}

fn parse_edge_types(s: Option<&str>) -> Result<Vec<EdgeType>, AppError> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(vec![]),
        Some(s) => s
            .split(',')
            .map(|t| EdgeType::parse(t.trim()))
            .collect::<engram_core::Result<_>>()
            .map_err(AppError::from),
    }
}

// ---- error mapping ------------------------------------------------------

pub enum AppError {
    Core(Error),
    Serde(serde_json::Error),
    NotFound,
}

impl From<Error> for AppError {
    fn from(e: Error) -> Self {
        AppError::Core(e)
    }
}
impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Serde(e)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            AppError::Core(Error::NotFound(s)) => (StatusCode::NOT_FOUND, s),
            AppError::Core(e @ (Error::Parse { .. } | Error::Project(_) | Error::Config(_))) => {
                (StatusCode::BAD_REQUEST, e.to_string())
            }
            AppError::Core(e @ Error::Pinned(_)) => (StatusCode::CONFLICT, e.to_string()),
            AppError::Core(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            AppError::Serde(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

#[cfg(test)]
mod tests;
