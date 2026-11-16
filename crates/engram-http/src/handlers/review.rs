//! The suspect queue, Checkup sweeps, sleep review views, decay and drift.

use crate::*;

pub(crate) async fn list_suspects(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<Vec<SuspectView>>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let suspects = pane(&engine).suspects()?;
    Ok(Json(suspects))
}

/// The Checkup panel's NLI scoreboard: how often the local model's suspect
/// hint agreed with the judge's actual verdict, over the judged history.
pub(crate) async fn nli_agreement(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<NliAgreement>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let report = pane(&engine).nli_agreement()?;
    Ok(Json(report))
}

/// Verified code refs: nodes whose path-shaped refs no longer
/// exist under the scoped project's root.
pub(crate) async fn drift(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<Vec<Drift>>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let root = state.scoped_root(&engine);
    let drifted = pane(&engine).scan_code_refs(&root)?;
    Ok(Json(drifted))
}

/// Crowding: per-note close-neighbour counts and heat for the
/// graph view, plus the unreviewed subjects and islands for Checkup.
pub(crate) async fn crowding(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<engram_core::CrowdingReport>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let report = pane(&engine).crowding_report()?;
    Ok(Json(report))
}

#[derive(Deserialize)]
pub(crate) struct ReviewParams {
    pub(crate) offset: Option<usize>,
    pub(crate) limit: Option<usize>,
    pub(crate) include_reviewed: Option<bool>,
    pub(crate) tag: Option<String>,
}

/// The sleep queue, the same page MCP's `list_nodes` `view: "review"` reads.
pub(crate) async fn review_page(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<ReviewParams>,
) -> Result<Json<engram_core::ReviewPage>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let page = pane(&engine).review_page(&engram_core::ReviewQuery {
        types: Vec::new(),
        tag: p.tag,
        include_reviewed: p.include_reviewed.unwrap_or(false),
        offset: p.offset.unwrap_or(0),
        limit: p.limit.unwrap_or(1).min(10),
    })?;
    Ok(Json(page))
}

/// Sleep's first queue: standing contradiction edges and pending suspects
/// (MCP: `list_nodes` `view: "conflicts"`).
pub(crate) async fn review_conflicts(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<engram_core::ConflictsView>, AppError> {
    let engine = state.engine_arc(&scope)?;
    Ok(Json(pane(&engine).conflicts_view()?))
}

/// Notes no edge and no tag reaches, with their nearest notes (MCP:
/// `list_nodes` `view: "islands"`).
pub(crate) async fn review_islands(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<ReviewParams>,
) -> Result<Json<engram_core::IslandPage>, AppError> {
    let engine = state.engine_arc(&scope)?;
    Ok(Json(pane(&engine).islands_page(
        p.include_reviewed.unwrap_or(false),
        p.offset.unwrap_or(0),
        p.limit.unwrap_or(20).min(100),
    )?))
}

#[derive(Deserialize)]
pub(crate) struct ReviewMarks {
    pub(crate) ids: Vec<String>,
    #[serde(default = "yes")]
    pub(crate) reviewed: bool,
}

pub(crate) fn yes() -> bool {
    true
}

/// Mark notes reviewed (a subject the user looked at and kept), or clear
/// the marks with `reviewed: false`.
pub(crate) async fn review_marks(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(p): Json<ReviewMarks>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let done = pane(&engine).mark_reviewed(&p.ids, p.reviewed)?;
    Ok(Json(json!({ "marked": done, "reviewed": p.reviewed })))
}

/// Digestion tier 1: the gitignore-aware FIXME/TODO scan of the
/// working tree. Candidates only — the digest skill judges them and writes
/// through the normal node path. Runs outside the engine lock: it never
/// touches the store. Deliberately HTTP-only, no MCP tool.
pub(crate) async fn digest_scan(
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

/// Audit-panel sweep: deep conflict pass (lower similarity floor, NLI-gated).
pub(crate) async fn audit_conflicts(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<AuditSweep>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let sweep = pane(&engine).audit_conflicts()?;
    Ok(Json(sweep))
}

/// Audit-panel sweep: mutual-entailment duplicates.
pub(crate) async fn audit_duplicates(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<AuditSweep>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let sweep = pane(&engine).audit_duplicates()?;
    Ok(Json(sweep))
}

/// Audit-panel check: open Problems/Intents that an existing node may answer.
pub(crate) async fn audit_answered(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<Vec<AnsweredHint>>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let hints = pane(&engine).audit_answered()?;
    Ok(Json(hints))
}

/// Promotion nominations: current-project Principles/Cautions
/// recurring in other projects, not yet represented in the home graph.
/// Read-only — the pane promotes via `POST /projects/home/nodes`.
pub(crate) async fn audit_promotions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (candidates, skipped) = state.hub.promotion_candidates()?;
    Ok(Json(
        json!({ "candidates": candidates, "skipped": skipped }),
    ))
}

/// Run the local candidate sweep on demand (the pane's "Scan now").
pub(crate) async fn scan_conflicts(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    // Off the async workers, one node per lock: a full scan with a
    // slow logic model takes a minute, and holding the engine for all of it
    // inside an async handler starved the whole core.
    drop(pane(&engine)); // attribute the scan's writes to the pane
    let added = tokio::task::spawn_blocking(move || Engine::scan_conflicts_shared(&engine))
        .await
        .map_err(|e| AppError::Core(Error::Io(format!("scan task: {e}"))))??;
    Ok(Json(json!({ "added": added })))
}

#[derive(Deserialize)]
pub(crate) struct ResolveBody {
    pub(crate) verdict: SuspectVerdict,
}

/// Judge a suspected pair from the pane — a user action, so edges it creates
/// are user-sourced.
pub(crate) async fn resolve_suspect(
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
pub(crate) struct DecayParams {
    pub(crate) ttl_days: Option<i64>,
    pub(crate) dry_run: Option<bool>,
}

pub(crate) async fn decay(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<DecayParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let ids = {
        let engine = engine.locked();
        let ttl = p
            .ttl_days
            .unwrap_or_else(|| engine.config().policy.decay_ttl_days);
        engine.decay(ttl, p.dry_run.unwrap_or(false))?
    };
    Ok(Json(json!({ "archived": ids.len(), "ids": ids })))
}

#[derive(Deserialize)]
pub(crate) struct RefsMatchParams {
    /// Repo-relative file path the assistant just read or edited.
    pub(crate) path: String,
    /// Acting session — the server deduplicates per session so the same
    /// node is never re-injected on every read (ambient value, not noise).
    #[serde(default)]
    pub(crate) session: Option<String>,
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}

/// The file-read match hook's lookup: nodes whose
/// code_refs cover the path, as ready-to-inject markdown. Noise control is
/// server-side — non-stale only, trust-ordered, capped, and per-session
/// deduplicated. An empty body means "nothing new to say".
pub(crate) async fn refs_match(
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
            let mut seen = state.refs_seen.locked();
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
pub(crate) async fn audit_stale(
    State(state): State<Arc<AppState>>,
    scope: Scope,
) -> Result<Json<Vec<engram_core::StaleTriage>>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let hints = pane(&engine).audit_stale_triage()?;
    Ok(Json(hints))
}
