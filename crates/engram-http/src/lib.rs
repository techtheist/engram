//! Thin axum API over `engram_core::Engine`: CRUD + hybrid search + the
//! whole-graph read the pane renders, plus a Server-Sent-Events stream that
//! pushes every mutation so the pane updates live (PLAN §6B).

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use engram_core::{
    ChangeEvent, Edge, EdgePatch, EdgeType, Engine, Error, ExportGraph, ImportSummary, NewEdge,
    NewNode, Node, NodePatch, NodeType, SearchHit, SuspectVerdict, SuspectView,
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::cors::CorsLayer;

/// Shared server state: the engine (serialized behind a `Mutex`, since the
/// SQLite connection is `!Sync`) and the live-update broadcast channel.
pub struct AppState {
    engine: Arc<Mutex<Engine>>,
    events: broadcast::Sender<String>,
    /// The database this daemon serves, reported by `/health` so a client that
    /// discovered a port can verify it belongs to *this* repo's daemon.
    db_path: Option<String>,
}

impl AppState {
    pub fn new(engine: Engine) -> Self {
        Self::shared(Arc::new(Mutex::new(engine)))
    }

    /// Build state around a shared engine and install the change listener that
    /// turns every mutation — from this API *or* from Claude over MCP — into an
    /// SSE message.
    pub fn shared(engine: Arc<Mutex<Engine>>) -> Self {
        Self::shared_with_db(engine, None)
    }

    pub fn shared_with_db(engine: Arc<Mutex<Engine>>, db_path: Option<String>) -> Self {
        let (events, _) = broadcast::channel(256);
        let tx = events.clone();
        engine.lock().unwrap().set_listener(Box::new(move |ev| {
            let _ = tx.send(encode_event(&ev));
        }));
        Self {
            engine,
            events,
            db_path,
        }
    }
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

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/nodes", post(create_node))
        .route(
            "/nodes/{id}",
            get(get_node).patch(patch_node).delete(delete_node),
        )
        .route("/nodes/{id}/edges", get(node_edges))
        .route("/nodes/{id}/reconfirm", post(reconfirm))
        .route("/nodes/{id}/approve", post(approve))
        .route("/nodes/{id}/traverse", get(traverse))
        .route("/edges", post(create_edge))
        .route(
            "/edges/{id}",
            axum::routing::patch(patch_edge).delete(delete_edge),
        )
        .route("/search", get(search))
        .route("/conflicts/suspects", get(list_suspects))
        .route("/conflicts/suspects/{id}/resolve", post(resolve_suspect))
        .route("/conflicts/scan", post(scan_conflicts))
        .route("/decay", post(decay))
        .route("/brief", get(brief))
        .route("/open", get(list_open))
        .route("/graph", get(graph))
        .route("/export", get(export))
        .route("/import", post(import))
        .route("/events", get(sse))
        // Anything not an API route is the Vue pane (served from the embedded
        // build), so `engram serve` is a complete browser-standalone app and
        // the IDE wrappers just point a webview at this one URL.
        .fallback(static_pane)
        .layer(CorsLayer::permissive())
        .with_state(state)
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

// ---- node handlers ------------------------------------------------------

async fn create_node(
    State(state): State<Arc<AppState>>,
    Json(input): Json<NewNode>,
) -> Result<Json<Node>, AppError> {
    let node = state.engine.lock().unwrap().add_node(input)?;
    Ok(Json(node))
}

async fn get_node(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Node>, AppError> {
    let node = state.engine.lock().unwrap().get_node(&id)?;
    node.map(Json).ok_or(AppError::NotFound)
}

async fn patch_node(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(patch): Json<NodePatch>,
) -> Result<Json<Node>, AppError> {
    let node = {
        let engine = state.engine.lock().unwrap();
        if engine.get_node(&id)?.is_none() {
            return Err(AppError::NotFound);
        }
        engine.update_node(&id, patch)?
    };
    Ok(Json(node))
}

async fn delete_node(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let removed = state.engine.lock().unwrap().delete_node(&id)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

async fn node_edges(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<EdgesResponse>, AppError> {
    let engine = state.engine.lock().unwrap();
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
    Path(id): Path<String>,
    Query(p): Query<TraverseParams>,
) -> Result<Json<GraphResponse>, AppError> {
    let edge_types = parse_edge_types(p.edge_types.as_deref())?;
    let depth = p.depth.unwrap_or(2);
    let (nodes, edges) = state
        .engine
        .lock()
        .unwrap()
        .traverse(&id, &edge_types, depth)?;
    Ok(Json(GraphResponse { nodes, edges }))
}

// ---- edge handler -------------------------------------------------------

async fn create_edge(
    State(state): State<Arc<AppState>>,
    Json(input): Json<NewEdge>,
) -> Result<Json<Edge>, AppError> {
    let edge = {
        let engine = state.engine.lock().unwrap();
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
    Path(id): Path<String>,
    Json(patch): Json<EdgePatch>,
) -> Result<Json<Edge>, AppError> {
    let edge = state.engine.lock().unwrap().update_edge(&id, patch)?;
    Ok(Json(edge))
}

async fn delete_edge(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let removed = state.engine.lock().unwrap().delete_edge(&id)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

// ---- search / read handlers --------------------------------------------

async fn search(
    State(state): State<Arc<AppState>>,
    Query(p): Query<SearchParams>,
) -> Result<Json<Vec<SearchHit>>, AppError> {
    let types = parse_node_types(p.types.as_deref())?;
    let limit = p.limit.unwrap_or(8);
    let hits = state.engine.lock().unwrap().search(&p.q, &types, limit)?;
    Ok(Json(hits))
}

// ---- conflict scan + decay (PLAN §7 / §6B) --------------------------------

async fn list_suspects(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<SuspectView>>, AppError> {
    let suspects = state.engine.lock().unwrap().suspects()?;
    Ok(Json(suspects))
}

/// Run the local candidate sweep on demand (the pane's "Scan now").
async fn scan_conflicts(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let added = state.engine.lock().unwrap().scan_conflicts()?;
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
    Path(id): Path<String>,
    Json(body): Json<ResolveBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let edge = state.engine.lock().unwrap().resolve_suspect(
        &id,
        body.verdict,
        engram_core::Source::User,
    )?;
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
    Query(p): Query<DecayParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let ttl = p.ttl_days.unwrap_or(engram_core::policy::DECAY_TTL_DAYS);
    let ids = state
        .engine
        .lock()
        .unwrap()
        .decay(ttl, p.dry_run.unwrap_or(false))?;
    Ok(Json(json!({ "archived": ids.len(), "ids": ids })))
}

/// The session-start digest, as `text/markdown` (PLAN §6A retrieval trigger).
async fn brief(
    State(state): State<Arc<AppState>>,
    Query(p): Query<BriefParams>,
) -> Result<Response, AppError> {
    let max_chars = p
        .max_chars
        .unwrap_or(engram_core::policy::DEFAULT_BRIEF_CHARS);
    let text = state.engine.lock().unwrap().brief(max_chars)?;
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/markdown; charset=utf-8",
        )],
        text,
    )
        .into_response())
}

async fn list_open(
    State(state): State<Arc<AppState>>,
    Query(p): Query<TypesParam>,
) -> Result<Json<Vec<Node>>, AppError> {
    let types = parse_node_types(p.types.as_deref())?;
    let nodes = state.engine.lock().unwrap().list_open(&types)?;
    Ok(Json(nodes))
}

async fn graph(State(state): State<Arc<AppState>>) -> Result<Json<GraphResponse>, AppError> {
    let (nodes, edges) = state.engine.lock().unwrap().graph()?;
    Ok(Json(GraphResponse { nodes, edges }))
}

async fn reconfirm(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Node>, AppError> {
    let exists = state.engine.lock().unwrap().get_node(&id)?.is_some();
    if !exists {
        return Err(AppError::NotFound);
    }
    let node = state.engine.lock().unwrap().reconfirm(&id)?;
    Ok(Json(node))
}

async fn approve(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Node>, AppError> {
    let exists = state.engine.lock().unwrap().get_node(&id)?.is_some();
    if !exists {
        return Err(AppError::NotFound);
    }
    let node = state.engine.lock().unwrap().approve(&id)?;
    Ok(Json(node))
}

async fn export(State(state): State<Arc<AppState>>) -> Result<Json<ExportGraph>, AppError> {
    let graph = state.engine.lock().unwrap().export()?;
    Ok(Json(graph))
}

async fn import(
    State(state): State<Arc<AppState>>,
    Json(graph): Json<ExportGraph>,
) -> Result<Json<ImportSummary>, AppError> {
    let summary = state.engine.lock().unwrap().import(graph)?;
    Ok(Json(summary))
}

async fn sse(
    State(state): State<Arc<AppState>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(state.events.subscribe())
        .filter_map(|msg| msg.ok().map(|s| Ok(Event::default().data(s))));
    Sse::new(stream).keep_alive(KeepAlive::default())
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
            AppError::Core(e @ Error::Parse { .. }) => (StatusCode::BAD_REQUEST, e.to_string()),
            AppError::Core(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            AppError::Serde(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

#[cfg(test)]
mod tests;
