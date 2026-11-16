//! Routing: the API, project scoping, CORS, the embedded pane and SSE.

use crate::*;

/// Lock a scoped engine and stamp the pane as the writer. Front-ends sharing
/// an engine re-stamp under the lock on every operation (MCP does the same).
pub(crate) fn pane(engine: &Arc<Mutex<Engine>>) -> MutexGuard<'_, Engine> {
    let mut guard = engine.locked();
    guard.set_audit_origin(AuditOrigin::pane());
    guard
}

pub(crate) fn channel(events: &EventMap, project_id: &str) -> broadcast::Sender<String> {
    events
        .locked()
        .entry(project_id.to_string())
        .or_insert_with(|| broadcast::channel(256).0)
        .clone()
}

pub(crate) fn encode_event(ev: &ChangeEvent) -> String {
    let (kind, data) = match ev {
        ChangeEvent::NodeAdded(n) => ("node_added", json!(n)),
        ChangeEvent::NodeUpdated(n) => ("node_updated", json!(n)),
        ChangeEvent::NodeDeleted(id) => ("node_deleted", json!({ "id": id })),
        ChangeEvent::EdgeAdded(e) => ("edge_added", json!(e)),
        ChangeEvent::EdgeUpdated(e) => ("edge_updated", json!(e)),
        ChangeEvent::EdgeDeleted(id) => ("edge_deleted", json!({ "id": id })),
        ChangeEvent::SuspectsChanged => ("suspects_changed", json!({})),
        ChangeEvent::ConfigChanged => ("config_changed", json!({})),
        ChangeEvent::TasksChanged => ("tasks_changed", json!({})),
    };
    json!({ "type": kind, "data": data }).to_string()
}

// ---- project scoping ----------------------------------------------------

/// The selector a `/projects/{sel}/…` URL carried, stashed by the rewrite
/// layer; absent on bare routes (= the launch project).
#[derive(Clone)]
pub(crate) struct ScopeSel(pub(crate) String);

pub(crate) struct Scope(pub(crate) Option<String>);

impl<S: Send + Sync> FromRequestParts<S> for Scope {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Scope(
            parts.extensions.get::<ScopeSel>().map(|s| s.0.clone()),
        ))
    }
}

/// Rewrite `/projects/{sel}/rest…` to `/rest…` plus a [`ScopeSel`] extension,
/// so the API exists once for every project. Two-segment paths are registry ops
/// and pass through. Wraps the router as a fallback service because
/// `Router::layer` middleware runs after routing.
pub(crate) async fn project_scope_rewrite(mut req: Request) -> Request {
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

/// The multi-project daemon: one router over a hub.
pub fn router_hub(hub: Arc<Hub>, db_path: Option<String>) -> Router {
    router(Arc::new(AppState::from_hub(hub, db_path)))
}

/// [`router_hub`] plus the daemon's model-selection hands.
pub fn router_hub_with_models(
    hub: Arc<Hub>,
    db_path: Option<String>,
    admin: Arc<dyn ModelAdmin>,
) -> Router {
    let mut state = AppState::from_hub(hub, db_path);
    state.model_admin = Some(admin);
    router(Arc::new(state))
}

/// [`router_hub_with_models`] plus skill-installation hands.
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

pub(crate) fn api_router(state: Arc<AppState>) -> Router {
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
        .route("/encryption", get(get_encryption).post(post_encryption))
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
        .route("/crowding", get(crowding))
        .route("/review", get(review_page))
        .route("/review/marks", post(review_marks))
        .route("/review/conflicts", get(review_conflicts))
        .route("/review/islands", get(review_islands))
        .route("/digest/scan", post(digest_scan))
        .route("/nodes/{id}/timeline", get(timeline))
        .route("/decay", post(decay))
        .route("/brief", get(brief))
        .route("/audit", get(audit))
        .route("/open", get(list_open))
        .route("/tasks", get(list_tasks).post(create_task))
        .route("/tasks/order", post(order_tasks))
        .route(
            "/tasks/{id}",
            get(get_task).patch(patch_task).delete(delete_task),
        )
        .route("/graph", get(graph))
        .route("/export", get(export))
        .route("/import", post(import))
        .route("/config", get(get_config).put(put_config))
        .route("/config/presets", get(config_presets))
        .route("/config/presets/{id}", post(apply_preset))
        .route("/guide", get(guide))
        .route("/version", get(get_version).put(put_version))
        .route("/config/rename-type", post(rename_type))
        .route("/config/rename-field", post(rename_field))
        .route("/config/rename-verb", post(rename_verb))
        .route("/skills/install", post(skills_install))
        .route("/refs/match", get(refs_match))
        .route("/history", get(history_stats).delete(history_reset))
        .route("/nodes/{id}/born-in", get(node_born_in))
        .route("/nodes/{id}/standing", get(node_standing))
        .route("/nodes/{id}/bridges", get(node_bridges))
        .route("/history/sessions", get(history_sessions))
        .route(
            "/history/sessions/{sid}",
            get(history_session_messages).delete(history_session_delete),
        )
        .route("/audit/stale", post(audit_stale))
        .route("/audit/{seq}/undo", post(audit_undo))
        .route("/audit/sessions/{sid}/undo", post(audit_undo_session))
        .route("/events", get(sse))
        // Anything not an API route is the Vue pane (served from the embedded
        // build), so `engram-alpha serve` is a complete browser-standalone app and
        // the IDE wrappers just point a webview at this one URL.
        .fallback(static_pane)
        .layer(local_cors())
        .with_state(state)
}

/// CORS for a loopback daemon (SECURITY.md): loopback pages and the IDE
/// webviews may read the API, other websites may not. Origin-less requests
/// (curl, JCEF, bridges) never hit CORS.
pub(crate) fn local_cors() -> CorsLayer {
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
pub(crate) struct Pane;

/// Serve an embedded asset by path; fall back to `index.html` so client-side
/// routes (and a bare `/`) resolve to the single-page app.
pub(crate) async fn static_pane(uri: axum::http::Uri) -> Response {
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

pub(crate) async fn sse(
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
