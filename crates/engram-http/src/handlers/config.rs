//! Per-graph config, versions, presets, renames, the guide and skill installs.

use crate::*;

pub(crate) async fn get_config(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<engram_core::GraphConfig>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let cfg = pane(&engine).graph_config();
    Ok(Json(cfg))
}

pub(crate) async fn put_config(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(cfg): Json<engram_core::GraphConfig>,
) -> Result<Json<engram_core::GraphConfig>, AppError> {
    let engine = state.engine_arc(&scope)?;
    pane(&engine).set_graph_config(&cfg)?;
    Ok(Json(cfg))
}

#[derive(Deserialize)]
pub(crate) struct PutVersionParams {
    pub(crate) version: Option<String>,
}

/// The graph's current working version (version tracking, 0.7.0).
pub(crate) async fn get_version(
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

pub(crate) async fn put_version(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(p): Json<PutVersionParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let previous = pane(&engine).set_current_version(p.version.as_deref())?;
    Ok(Json(json!({ "previous": previous, "current": p.version })))
}

/// The operator's guide: every settings surface as HTTP recipes, for an
/// assistant changing how a graph behaves at the user's request.
pub const OPERATOR_GUIDE: &str = include_str!("../guide.md");

pub(crate) async fn guide() -> Response {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/markdown; charset=utf-8",
        )],
        OPERATOR_GUIDE,
    )
        .into_response()
}

/// Apply a shipped preset to THIS graph: its ontology replaces the
/// graph's, and the graph's policy (auto-tuned values included), brief,
/// versioning, history and field definitions are kept. Answers the
/// resulting document, like `PUT /config`.
pub(crate) async fn apply_preset(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<Json<engram_core::GraphConfig>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let cfg = pane(&engine).apply_preset(&id)?;
    Ok(Json(cfg))
}

/// The curated ontology templates — machine-level data,
/// same shelf for every project; `POST /config/presets/{id}` applies one.
pub(crate) async fn config_presets() -> Json<Vec<engram_core::config::Preset>> {
    Json(engram_core::config::presets())
}

#[derive(Deserialize)]
pub(crate) struct RenameParams {
    pub(crate) from: String,
    pub(crate) to: String,
}

#[derive(Deserialize)]
pub(crate) struct SkillInstallParams {
    #[serde(default)]
    pub(crate) variant: Option<String>,
}

/// (Re)install the assistant capture skill into the scoped project's repo —
/// generated from the graph's ontology when it is customized, the canonical
/// variant text when it runs the shipped set.
pub(crate) async fn skills_install(
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
        let engine = engine.locked();
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
pub(crate) async fn rename_type(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(p): Json<RenameParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let renamed = pane(&engine).rename_type(&p.from, &p.to)?;
    Ok(Json(json!({ "renamed": renamed })))
}

/// Rename a custom field, moving every stored value with it — the
/// field analog of rename-type.
pub(crate) async fn rename_field(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(p): Json<RenameParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let renamed = pane(&engine).rename_field(&p.from, &p.to)?;
    Ok(Json(json!({ "renamed": renamed })))
}

/// Rename an edge verb and bulk-retype its stored edges.
pub(crate) async fn rename_verb(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(p): Json<RenameParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let renamed = pane(&engine).rename_verb(&p.from, &p.to)?;
    Ok(Json(json!({ "renamed": renamed })))
}
