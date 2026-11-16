//! Shared query parameters.

use crate::*;

#[derive(Deserialize)]
pub(crate) struct SearchParams {
    pub(crate) q: String,
    pub(crate) limit: Option<usize>,
    pub(crate) types: Option<String>,
    /// "history" = search the recorded-session layer instead of curated
    /// memory (the pane's history view routes its search box here).
    pub(crate) scope: Option<String>,
    /// The temporal grammar, same shapes the MCP surface takes: a day,
    /// an ISO instant, or a relative expression the daemon resolves.
    pub(crate) after: Option<String>,
    pub(crate) before: Option<String>,
    pub(crate) during_version: Option<String>,
    /// "relevance" (default) | "chronological" | "recent".
    pub(crate) order: Option<String>,
    /// Bitemporal search: a date-kind custom field ("event_date")
    /// or a "from..to" pair with interval-overlap semantics — aims the
    /// after/before window at the graph's event clock instead of created_at.
    pub(crate) date_field: Option<String>,
}

/// A bare time window for the browsing endpoints — same grammar as
/// search's `after`/`before`.
#[derive(Deserialize)]
pub(crate) struct WindowParams {
    pub(crate) after: Option<String>,
    pub(crate) before: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct TypesParam {
    pub(crate) types: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct TraverseParams {
    pub(crate) edge_types: Option<String>,
    pub(crate) depth: Option<usize>,
}

#[derive(Deserialize)]
pub(crate) struct BriefParams {
    pub(crate) max_chars: Option<usize>,
    /// Brief AS this project — a name, an id, or the project's directory
    /// (any path inside it). The session-brief hook holds a folder, not a
    /// name, and the machine core's launch graph is nobody's project.
    pub(crate) project: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct TagsParams {
    pub(crate) limit: Option<usize>,
}

#[derive(Deserialize)]
pub(crate) struct UndoParams {
    #[serde(default)]
    pub(crate) dry_run: bool,
}

#[derive(Deserialize)]
pub(crate) struct AuditParams {
    pub(crate) limit: Option<usize>,
    pub(crate) before: Option<i64>,
    pub(crate) entity_id: Option<String>,
}

pub(crate) fn parse_node_types(s: Option<&str>) -> Result<Vec<NodeType>, AppError> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(vec![]),
        Some(s) => s
            .split(',')
            .map(|t| NodeType::parse(t.trim()))
            .collect::<engram_core::Result<_>>()
            .map_err(AppError::from),
    }
}

pub(crate) fn parse_edge_types(s: Option<&str>) -> Result<Vec<EdgeType>, AppError> {
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
