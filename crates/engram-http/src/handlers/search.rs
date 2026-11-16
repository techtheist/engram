//! Search, tags, the brief, open work and claim checks.

use crate::*;

pub(crate) async fn search(
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
    if p.scope.as_deref() == Some("history")
        && p.date_field
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
    {
        return Err(AppError::Core(Error::Config(
            "date_field applies to curated memory — history has no custom fields".into(),
        )));
    }
    let filter = engine.locked().time_filter_clocked(
        p.after.as_deref(),
        p.before.as_deref(),
        p.during_version.as_deref(),
        p.order.as_deref(),
        p.date_field.as_deref(),
    )?;
    if p.scope.as_deref() == Some("history") {
        let hits = engine
            .locked()
            .search_history_filtered(&p.q, limit, &filter)?;
        return Ok(Json(json!(hits)));
    }
    let hits = pane(&engine).search_filtered(&p.q, &types, limit, &filter)?;
    Ok(Json(json!(hits)))
}

/// Tags in use on current nodes, freshest first — feeds the pane's tag
/// dropdown and filter chips.
pub(crate) async fn tags(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<TagsParams>,
) -> Result<Json<Vec<TagStat>>, AppError> {
    let limit = p.limit.unwrap_or(200);
    let engine = state.engine_arc(&scope)?;
    let tags = pane(&engine).tags(limit)?;
    Ok(Json(tags))
}

// ---- conflict scan + decay --------------------------------

#[derive(Deserialize)]
pub(crate) struct CheckClaimBody {
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}

/// Verify a claim against the canon: supports / contradicts /
/// silent, each with the judging node. Requires the local NLI model.
/// Scope `all` judges across every reachable graph with provenance.
pub(crate) async fn check_claim(
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

/// The session-start digest as `text/markdown`. `/projects/{sel}/brief` is
/// that graph alone; `/brief?project=…` is what a session bound there gets
/// (plus the home section and roster), which the SessionStart hook injects;
/// plain `/brief` is that for the launch project.
pub(crate) async fn brief(
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
    let max_chars = engine.locked().brief_chars(p.max_chars);
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

pub(crate) async fn list_open(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<TypesParam>,
) -> Result<Json<Vec<Node>>, AppError> {
    let types = parse_node_types(p.types.as_deref())?;
    let engine = state.engine_arc(&scope)?;
    let nodes = pane(&engine).list_open(&types)?;
    Ok(Json(nodes))
}

// ---- tasks: the working list beside the graph -------------------
