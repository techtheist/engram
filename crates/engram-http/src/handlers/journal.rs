//! The audit journal and undo.

use crate::*;

/// One page of the audit journal, newest first. Keyset pagination:
/// pass the last entry's `seq` as `before` for the next page; `entity_id`
/// narrows to one node/edge's history.
pub(crate) async fn audit(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<AuditParams>,
) -> Result<Json<AuditPage>, AppError> {
    let limit = p.limit.unwrap_or(50).min(200);
    let engine = state.engine_arc(&scope)?;
    let page = pane(&engine).audit_log(p.before, p.entity_id.as_deref(), limit)?;
    Ok(Json(page))
}

/// Undo one journal row — or its whole operation — from the pane.
/// User-only like hard delete: HTTP serves it, MCP has no such tool.
/// `?dry_run=true` reports what would happen without changing anything.
pub(crate) async fn audit_undo(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(seq): Path<String>,
    Query(p): Query<UndoParams>,
) -> Result<Json<engram_core::UndoReport>, AppError> {
    let seq: i64 = seq
        .parse()
        .map_err(|_| engram_core::Error::NotFound(format!("audit row {seq:?}")))?;
    let engine = state.engine_arc(&scope)?;
    let report = pane(&engine).undo_entry(seq, p.dry_run)?;
    Ok(Json(report))
}

/// Undo everything one session wrote — see [`audit_undo`].
pub(crate) async fn audit_undo_session(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(sid): Path<String>,
    Query(p): Query<UndoParams>,
) -> Result<Json<engram_core::UndoReport>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let report = pane(&engine).undo_session(&sid, p.dry_run)?;
    Ok(Json(report))
}
