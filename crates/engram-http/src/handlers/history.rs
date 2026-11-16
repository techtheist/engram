//! The history layer: stats, sessions, provenance and reset.

use crate::*;

/// The decay pass. `dry_run=true` previews what would archive.
/// History-layer status for the pane's settings section: whether the layer
/// is open, and what it holds. Cheap enough to poll.
pub(crate) async fn history_stats(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let engine = engine.locked();
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
pub(crate) async fn history_reset(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    {
        let mut engine = engine.locked();
        engine.set_audit_origin(engram_core::AuditOrigin::pane());
        engine.reset_history()?;
    }
    state.hub.bump_history_epoch();
    Ok(Json(json!({ "reset": true })))
}

/// The curated node's birth exchange, when born-in provenance exists — the
/// pane's "history" chip.
pub(crate) async fn node_born_in(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let born = engine.locked().born_in_of(&id);
    Ok(Json(json!({ "born_in": born })))
}

pub(crate) async fn history_sessions(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<WindowParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let window =
        engram_core::timespec::window(p.after.as_deref(), p.before.as_deref(), engram_core::now())?;
    let sessions = engine.locked().list_history_sessions_in(window)?;
    Ok(Json(json!({ "sessions": sessions })))
}

pub(crate) async fn history_session_messages(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(sid): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let messages = engine.locked().history_messages(&sid)?;
    Ok(Json(json!({ "session": sid, "messages": messages })))
}

/// Per-session hard delete — pane-only, like every hard delete.
pub(crate) async fn history_session_delete(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(sid): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let removed = {
        let mut engine = engine.locked();
        engine.set_audit_origin(AuditOrigin::pane());
        // No epoch bump: the deletion excludes the transcript path itself,
        // and a cursor wipe would only re-scan what's now excluded anyway.
        engine.delete_history_session(&sid)?
    };
    Ok(Json(json!({ "removed": removed })))
}
