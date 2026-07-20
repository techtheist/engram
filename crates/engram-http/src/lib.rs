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
    EdgePatch, EdgeType, Engine, Error, ExportGraph, Hub, ImportSummary, NewEdge, NewNode, Node,
    NodePatch, NodeType, ProjectInfo, SuspectVerdict, SuspectView, TagStat, TimelineEntry,
    registry,
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
        let events: EventMap = Arc::default();
        let ev = events.clone();
        hub.set_listener_factory(Box::new(move |project_id: &str| {
            let tx = channel(&ev, project_id);
            Box::new(move |change| {
                let _ = tx.send(encode_event(&change));
            })
        }));
        Self {
            hub,
            events,
            db_path,
            started: std::time::Instant::now(),
            model_admin: None,
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

fn api_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/system", get(system))
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
        .route("/events", get(sse))
        // Anything not an API route is the Vue pane (served from the embedded
        // build), so `engram-alpha serve` is a complete browser-standalone app and
        // the IDE wrappers just point a webview at this one URL.
        .fallback(static_pane)
        .layer(CorsLayer::permissive())
        .with_state(state)
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

    Json(json!({
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
    }))
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
    let hits = pane(&engine).search(&p.q, &types, limit)?;
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
async fn decay(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<DecayParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let ttl = p.ttl_days.unwrap_or(engram_core::policy::DECAY_TTL_DAYS);
    let engine = state.engine_arc(&scope)?;
    let ids = engine
        .lock()
        .unwrap()
        .decay(ttl, p.dry_run.unwrap_or(false))?;
    Ok(Json(json!({ "archived": ids.len(), "ids": ids })))
}

/// The session-start digest, as `text/markdown` (PLAN §6A retrieval trigger).
/// Unscoped = the launch project plus the home-graph section; a scoped
/// project (or `home`) briefs that graph alone.
async fn brief(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<BriefParams>,
) -> Result<Response, AppError> {
    let max_chars = p
        .max_chars
        .unwrap_or(engram_core::policy::DEFAULT_BRIEF_CHARS);
    let text = match &scope.0 {
        None => state.hub.brief(max_chars)?,
        Some(_) => {
            let engine = state.engine_arc(&scope)?;
            pane(&engine).brief(max_chars)?
        }
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
            AppError::Core(e @ (Error::Parse { .. } | Error::Project(_))) => {
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
