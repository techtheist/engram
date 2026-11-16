//! Nodes, edges, trust acts, the whole graph, export and import.

use crate::*;

pub(crate) async fn create_node(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(input): Json<NewNode>,
) -> Result<Json<Node>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let node = pane(&engine).add_node(input)?;
    Ok(Json(node))
}

pub(crate) async fn get_node(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<Json<Node>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let node = pane(&engine).get_node(&id)?;
    node.map(Json).ok_or(AppError::NotFound)
}

pub(crate) async fn patch_node(
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

#[derive(Deserialize, Default)]
pub(crate) struct DeleteNodeParams {
    /// Leave a tombstone-role note recording the removal. Ignored —
    /// a plain delete — when the graph's ontology declares no tombstone type.
    #[serde(default)]
    pub(crate) tombstone: bool,
    /// Why the node was removed; lands in the tombstone's body.
    #[serde(default)]
    pub(crate) reason: Option<String>,
    /// Carry the victim's body, tags, and code_refs into the tombstone so a
    /// paraphrase of the removed content still lands near it.
    /// Default true; `keep_text=false` is the purge shape — identity and
    /// reason only.
    #[serde(default = "default_true")]
    pub(crate) keep_text: bool,
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) async fn delete_node(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
    Query(params): Query<DeleteNodeParams>,
) -> Result<Response, AppError> {
    let engine = state.engine_arc(&scope)?;
    if params.tombstone {
        let (removed, tombstone) = pane(&engine).delete_node_with_tombstone(
            &id,
            params.reason.as_deref(),
            params.keep_text,
        )?;
        if !removed {
            return Err(AppError::NotFound);
        }
        return Ok(
            Json(serde_json::json!({ "deleted": true, "tombstone": tombstone })).into_response(),
        );
    }
    let removed = pane(&engine).delete_node(&id)?;
    if removed {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Err(AppError::NotFound)
    }
}

pub(crate) async fn node_edges(
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

pub(crate) async fn traverse(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
    Query(p): Query<TraverseParams>,
) -> Result<Json<GraphResponse>, AppError> {
    let edge_types = parse_edge_types(p.edge_types.as_deref())?;
    let depth = p.depth.unwrap_or(2);
    let engine = state.engine_arc(&scope)?;
    let (nodes, edges) = engine.locked().traverse(&id, &edge_types, depth)?;
    Ok(Json(GraphResponse { nodes, edges }))
}

// ---- edge handler -------------------------------------------------------

pub(crate) async fn create_edge(
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

pub(crate) async fn patch_edge(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
    Json(patch): Json<EdgePatch>,
) -> Result<Json<Edge>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let edge = pane(&engine).update_edge(&id, patch)?;
    Ok(Json(edge))
}

pub(crate) async fn delete_edge(
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

/// Timeline: the node's `replaces` chain, oldest first.
pub(crate) async fn timeline(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<Json<Vec<TimelineEntry>>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let chain = pane(&engine).timeline(&id)?;
    Ok(Json(chain))
}

/// The grounded-answer verdict for one node — the same `standing`
/// search hits carry, for the pane's inspector.
pub(crate) async fn node_standing(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<Json<engram_core::Standing>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let standing = pane(&engine).node_standing(&id)?;
    standing.map(Json).ok_or(AppError::NotFound)
}

/// Found bridges for the pane's detail view: notes a bridge note
/// ties to this one while no edge does. Read on open, never stored.
pub(crate) async fn node_bridges(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let bridges = pane(&engine).found_bridges(&id, NODE_BRIDGES)?;
    Ok(Json(json!({ "bridges": bridges })))
}

/// Found bridges the detail view lists — a little more room than the MCP
/// reply's three, the pane is not paying for context.
pub(crate) const NODE_BRIDGES: usize = 6;

pub(crate) async fn graph(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<GraphResponse>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let (nodes, edges) = pane(&engine).graph()?;
    Ok(Json(GraphResponse { nodes, edges }))
}

pub(crate) async fn reconfirm(
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

pub(crate) async fn approve(
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
pub(crate) async fn revoke_approval(
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
pub(crate) struct PinBody {
    /// Constant trust in 0..=1 (pin = 1.0); null clears the pin.
    pub(crate) value: Option<f64>,
}

/// Set or clear the constant-trust pin (trust v2). User-only, like the
/// hard delete — the MCP server deliberately exposes no counterpart.
pub(crate) async fn pin(
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

pub(crate) async fn export(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<ExportGraph>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let graph = pane(&engine).export()?;
    Ok(Json(graph))
}

pub(crate) async fn import(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(graph): Json<ExportGraph>,
) -> Result<Json<ImportSummary>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let summary = pane(&engine).import(graph)?;
    Ok(Json(summary))
}
