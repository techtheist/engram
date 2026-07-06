//! stdio MCP server (`rmcp`) exposing the Engram graph to Claude. Thin wrapper
//! over `engram_core::Engine` implementing the Appendix A tool contracts. Note:
//! `delete_node` is deliberately absent — hard delete is user-only (PLAN §6B),
//! so Claude has no tool for it.

use std::sync::{Arc, Mutex};

use engram_core::{
    Durability, EdgePatch, EdgeStatus, EdgeType, Engine, Error, NewEdge, NewNode, NodePatch,
    NodeStatus, NodeType, Source, SuspectVerdict, WriteOutcome,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

const INSTRUCTIONS: &str = "\
Engram is the project's durable reasoning/decision memory as an editable graph. \
Call `brief` at the start of a session for a compact digest of the canon \
(conflicts, open work, principles, decisions, cautions). Use `search` before \
non-trivial work — hits carry their 1-hop neighbors, conflicts and supersessions \
first. `add_note` self-checks for near-duplicates (returns {matched, \
created:false} — then merge via `update_node`) and both writes return warnings \
when the new text lands near contradicted or superseded knowledge: read them. \
Link nodes with sentence-shaped edges (e.g. a Decision `because` a Principle); \
repair a wrong link with `unlink` / `update_edge`. When the brief lists \
suspected conflicts, judge them early via `resolve_suspect` (conflict | \
replaces | dismiss) — the scan only finds candidates; you are the judge. \
Nodes carry computed \
`trust` (0..1, from created_at/last_seen/approved_at) and `stale` (trust < \
0.3 — verify before relying; refresh with `update_node` if still true). \
Never store secrets or volatile implementation detail.";

#[derive(Clone)]
pub struct Engram {
    engine: Arc<Mutex<Engine>>,
    /// Fallback session id when the client omits one: minted once per server
    /// process, which over stdio is one Claude session. Superseded by the
    /// transport session id after the streamable-HTTP migration (PLAN §0).
    session_id: Arc<str>,
}

#[tool_router]
impl Engram {
    pub fn new(engine: Engine) -> Self {
        Self::with_shared(Arc::new(Mutex::new(engine)))
    }

    /// Build over an engine shared with the HTTP server (same DB + listener).
    pub fn with_shared(engine: Arc<Mutex<Engine>>) -> Self {
        Self {
            engine,
            session_id: format!("mcp-{}", engram_core::id::new_id()).into(),
        }
    }

    #[tool(
        description = "Hybrid semantic + keyword search over the memory graph. \
        Hits carry: type, title, snippet, score, trust (computed 0..1), stale \
        (true = decayed trust, verify before relying), status, and 1-hop \
        neighbors (conflicts-with/replaces first). Being returned refreshes a \
        node's last_seen."
    )]
    async fn search(
        &self,
        Parameters(a): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let types = node_types(&a.types)?;
        let hits = self
            .engine
            .lock()
            .unwrap()
            .search(&a.query, &types, a.limit.unwrap_or(8))
            .map_err(map_err)?;
        ok_json(&hits)
    }

    #[tool(
        description = "Fetch one node by id with its outgoing and incoming edges. \
        Node fields include computed trust (0..1) and stale (true = trust < 0.3). \
        Optional `parents`/`children` (depth 0-3) also return the reasoning \
        hierarchy: parents are nodes this one points at (its reasons/subjects — \
        e.g. the Principle behind a Decision); children are nodes pointing at it \
        (what answers / builds on it). Nested as {edge, node, parents|children}."
    )]
    async fn get_node(
        &self,
        Parameters(a): Parameters<GetNodeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine.lock().unwrap();
        let Some(node) = engine.get_node(&a.id).map_err(map_err)? else {
            return Err(ErrorData::invalid_params(
                format!("node not found: {}", a.id),
                None,
            ));
        };
        let out = engine.edges_out(&a.id).map_err(map_err)?;
        let incoming = engine.edges_in(&a.id).map_err(map_err)?;
        let mut payload = json!({ "node": node, "edges_out": out, "edges_in": incoming });
        let up = a.parents.unwrap_or(0).min(HIERARCHY_MAX_DEPTH);
        let down = a.children.unwrap_or(0).min(HIERARCHY_MAX_DEPTH);
        if up > 0 {
            let mut seen = std::collections::HashSet::from([a.id.clone()]);
            payload["parents"] = json!(hierarchy(&engine, &a.id, up, true, &mut seen));
        }
        if down > 0 {
            let mut seen = std::collections::HashSet::from([a.id.clone()]);
            payload["children"] = json!(hierarchy(&engine, &a.id, down, false, &mut seen));
        }
        ok_json(&payload)
    }

    #[tool(
        description = "Bounded breadth-first subgraph around a node, optionally \
        filtered to specific edge types."
    )]
    async fn traverse(
        &self,
        Parameters(a): Parameters<TraverseArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let edge_types = edge_types(&a.edge_types)?;
        let (nodes, edges) = self
            .engine
            .lock()
            .unwrap()
            .traverse(&a.from, &edge_types, a.depth.unwrap_or(2))
            .map_err(map_err)?;
        ok_json(&json!({ "nodes": nodes, "edges": edges }))
    }

    #[tool(
        description = "Create a memory node (source = claude, starts provisional). \
        Self-checks for a same-type near-duplicate and returns {matched, created: false} \
        instead of creating one — merge via update_node in that case. A created note \
        may carry `warnings` when it lands near contradicted or superseded knowledge."
    )]
    async fn add_note(
        &self,
        Parameters(a): Parameters<AddNoteArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let node_type = NodeType::parse(&a.node_type).map_err(map_err)?;
        let durability = match a.durability {
            Some(d) => Durability::parse(&d).map_err(map_err)?,
            None => default_durability(node_type),
        };
        let status = match node_type {
            NodeType::Problem | NodeType::Intent => Some(NodeStatus::Open),
            _ => None,
        };
        let outcome = self
            .engine
            .lock()
            .unwrap()
            .add_node_checked(NewNode {
                node_type,
                title: a.title,
                body: a.body,
                durability,
                source: Source::Claude,
                session_id: a.session_id.or_else(|| Some(self.session_id.to_string())),
                status,
                code_refs: a.code_refs,
            })
            .map_err(map_err)?;
        match outcome {
            WriteOutcome::Created { node, warnings } if warnings.is_empty() => {
                ok_json(&json!({ "id": node.id, "created": true }))
            }
            WriteOutcome::Created { node, warnings } => {
                ok_json(&json!({ "id": node.id, "created": true, "warnings": warnings }))
            }
            WriteOutcome::Matched { node, similarity } => ok_json(&json!({
                "matched": node.id,
                "created": false,
                "title": node.title,
                "similarity": similarity,
            })),
        }
    }

    #[tool(
        description = "Session-start digest of the memory graph as markdown: unresolved \
        conflicts, open problems/intents, principles, decisions, cautions, recent changes \
        — token-budgeted. Call this once when starting work on the project."
    )]
    async fn brief(
        &self,
        Parameters(a): Parameters<BriefArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let text = self
            .engine
            .lock()
            .unwrap()
            .brief(
                a.max_chars
                    .unwrap_or(engram_core::policy::DEFAULT_BRIEF_CHARS),
            )
            .map_err(map_err)?;
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(description = "Delete one edge by id — for repairing a mislink. \
        Nodes are never deleted this way (hard node delete is user-only).")]
    async fn unlink(&self, Parameters(a): Parameters<IdArg>) -> Result<CallToolResult, ErrorData> {
        let removed = self
            .engine
            .lock()
            .unwrap()
            .delete_edge(&a.id)
            .map_err(map_err)?;
        if !removed {
            return Err(ErrorData::invalid_params(
                format!("edge not found: {}", a.id),
                None,
            ));
        }
        ok_json(&json!({ "ok": true }))
    }

    #[tool(
        description = "Update an edge's status (active | resolved | dismissed), \
        note, or confidence — e.g. mark a conflicts-with as resolved."
    )]
    async fn update_edge(
        &self,
        Parameters(a): Parameters<UpdateEdgeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let patch = EdgePatch {
            status: a
                .status
                .map(|s| EdgeStatus::parse(&s))
                .transpose()
                .map_err(map_err)?,
            note: a.note,
            confidence: a.confidence,
            strength: None,
        };
        let edge = self
            .engine
            .lock()
            .unwrap()
            .update_edge(&a.id, patch)
            .map_err(map_err)?;
        ok_json(&json!({ "ok": true, "id": edge.id }))
    }

    #[tool(description = "Link two nodes with a sentence-shaped edge \
        (about, because, answers, builds-on, replaces, conflicts-with, needs).")]
    async fn link(&self, Parameters(a): Parameters<LinkArgs>) -> Result<CallToolResult, ErrorData> {
        let edge_type = EdgeType::parse(&a.edge_type).map_err(map_err)?;
        let edge = self
            .engine
            .lock()
            .unwrap()
            .add_edge(NewEdge {
                edge_type,
                from_id: a.from,
                to_id: a.to,
                source: Source::Claude,
                note: a.note,
                confidence: a.confidence,
                strength: None,
                status: None,
            })
            .map_err(map_err)?;
        ok_json(&json!({ "id": edge.id }))
    }

    #[tool(
        description = "Pending suspected conflicts from the local scan: unlinked \
        look-alike node pairs awaiting judgment. Judge each with resolve_suspect."
    )]
    async fn list_suspects(&self) -> Result<CallToolResult, ErrorData> {
        let suspects = self.engine.lock().unwrap().suspects().map_err(map_err)?;
        ok_json(&json!({ "suspects": suspects }))
    }

    #[tool(
        description = "Judge a suspected conflict: verdict `conflict` records a \
        conflicts-with edge, `replaces` records a replaces edge AND archives the \
        older node, `dismiss` marks the pair fine-together (never re-raised)."
    )]
    async fn resolve_suspect(
        &self,
        Parameters(a): Parameters<ResolveSuspectArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let verdict = SuspectVerdict::parse(&a.verdict).map_err(map_err)?;
        let edge = self
            .engine
            .lock()
            .unwrap()
            .resolve_suspect(&a.id, verdict, Source::Claude)
            .map_err(map_err)?;
        ok_json(&json!({ "ok": true, "edge": edge }))
    }

    #[tool(description = "Approve a node: trust restarts at 100% on the slow \
        one-year curve. ONLY on explicit user demand, or after verifying the \
        node's content word-by-word against current reality. Routine \
        still-relevant signals belong in update_node/reconfirm, not here.")]
    async fn approve_node(
        &self,
        Parameters(a): Parameters<ApproveArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let node = self
            .engine
            .lock()
            .unwrap()
            .approve(&a.id)
            .map_err(map_err)?;
        ok_json(&json!({ "ok": true, "id": node.id, "trust": node.trust }))
    }

    #[tool(
        description = "Update fields on an existing node (merge / reclassify / \
        refresh its trust via last_seen). Re-embeds when title or body changes."
    )]
    async fn update_node(
        &self,
        Parameters(a): Parameters<UpdateArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let patch = NodePatch {
            node_type: a
                .node_type
                .map(|t| NodeType::parse(&t))
                .transpose()
                .map_err(map_err)?,
            title: a.title,
            body: a.body,
            durability: a
                .durability
                .map(|d| Durability::parse(&d))
                .transpose()
                .map_err(map_err)?,
            status: a
                .status
                .map(|s| NodeStatus::parse(&s))
                .transpose()
                .map_err(map_err)?,
            valid_until: None,
            code_refs: a.code_refs,
        };
        let (node, warnings) = self
            .engine
            .lock()
            .unwrap()
            .update_node_checked(&a.id, patch)
            .map_err(map_err)?;
        if warnings.is_empty() {
            ok_json(&json!({ "ok": true, "id": node.id }))
        } else {
            ok_json(&json!({ "ok": true, "id": node.id, "warnings": warnings }))
        }
    }

    #[tool(description = "List the live worklist: open Problems and Intents.")]
    async fn list_open(
        &self,
        Parameters(a): Parameters<ListOpenArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let types = node_types(&a.types)?;
        let nodes = self
            .engine
            .lock()
            .unwrap()
            .worklist(&types, a.include_conflicts.unwrap_or(true))
            .map_err(map_err)?;
        ok_json(&nodes)
    }
}

#[tool_handler]
impl ServerHandler for Engram {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("engram", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS.to_string())
    }
}

/// Serve the MCP protocol over stdio until the client disconnects.
pub async fn serve_stdio(engine: Engine) -> anyhow::Result<()> {
    serve(Engram::new(engine)).await
}

/// Serve over stdio using an engine shared with the HTTP server.
pub async fn serve_stdio_shared(engine: Arc<Mutex<Engine>>) -> anyhow::Result<()> {
    serve(Engram::with_shared(engine)).await
}

async fn serve(server: Engram) -> anyhow::Result<()> {
    let service = server.serve(rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

// ---- argument schemas ---------------------------------------------------

#[derive(Deserialize, JsonSchema)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    types: Vec<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
struct IdArg {
    id: String,
}

#[derive(Deserialize, JsonSchema)]
struct GetNodeArgs {
    id: String,
    /// Levels of parent hierarchy to include (nodes this one points at), 0-3.
    #[serde(default)]
    parents: Option<usize>,
    /// Levels of child hierarchy to include (nodes pointing at this one), 0-3.
    #[serde(default)]
    children: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
struct TraverseArgs {
    from: String,
    #[serde(default)]
    edge_types: Vec<String>,
    #[serde(default)]
    depth: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
struct AddNoteArgs {
    #[serde(rename = "type")]
    node_type: String,
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    durability: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    code_refs: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
struct LinkArgs {
    from: String,
    to: String,
    #[serde(rename = "type")]
    edge_type: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
struct UpdateArgs {
    id: String,
    /// Reclassify the node (one of the 8 canonical types).
    #[serde(default, rename = "type")]
    node_type: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    durability: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    code_refs: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
struct ApproveArgs {
    id: String,
}

#[derive(Deserialize, JsonSchema)]
struct ResolveSuspectArgs {
    /// The suspect id (from the brief's "Suspected conflicts" section or list_suspects).
    id: String,
    /// "conflict" | "replaces" | "dismiss"
    verdict: String,
}

#[derive(Deserialize, JsonSchema)]
struct ListOpenArgs {
    #[serde(default)]
    types: Vec<String>,
    #[serde(default)]
    include_conflicts: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
struct BriefArgs {
    /// Character budget for the digest (default ~12000, about 3k tokens).
    #[serde(default)]
    max_chars: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
struct UpdateEdgeArgs {
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
}

// ---- helpers ------------------------------------------------------------

fn ok_json<T: Serialize>(v: &T) -> Result<CallToolResult, ErrorData> {
    let text = serde_json::to_string_pretty(v)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

const HIERARCHY_MAX_DEPTH: usize = 3;
const HIERARCHY_MAX_BREADTH: usize = 8;

/// Recursive reasoning hierarchy around a node. `up` follows outgoing edges
/// (parents: what this node stands on / is about); `!up` follows incoming
/// (children: what answers, builds on, or contradicts it). Depth and breadth
/// are capped and cycles cut so the payload stays context-window friendly.
fn hierarchy(
    engine: &engram_core::Engine,
    id: &str,
    depth: usize,
    up: bool,
    seen: &mut std::collections::HashSet<String>,
) -> Vec<serde_json::Value> {
    if depth == 0 {
        return Vec::new();
    }
    let edges = if up {
        engine.edges_out(id)
    } else {
        engine.edges_in(id)
    }
    .unwrap_or_default();
    let mut out = Vec::new();
    for e in edges.into_iter().take(HIERARCHY_MAX_BREADTH) {
        let other = if up { &e.to_id } else { &e.from_id };
        if !seen.insert(other.clone()) {
            continue;
        }
        let Ok(Some(n)) = engine.get_node(other) else {
            continue;
        };
        let deeper = hierarchy(engine, other, depth - 1, up, seen);
        let mut item = json!({
            "edge": e.edge_type.as_str(),
            "node": {
                "id": n.id,
                "type": n.node_type.as_str(),
                "title": n.title,
                "status": n.status.map(|s| s.as_str()),
                "trust": (n.trust * 100.0).round() / 100.0,
                "stale": n.stale,
                "archived": n.valid_until.is_some(),
            }
        });
        if !deeper.is_empty() {
            item[if up { "parents" } else { "children" }] = json!(deeper);
        }
        out.push(item);
    }
    out
}

fn map_err(e: Error) -> ErrorData {
    match e {
        Error::NotFound(s) => ErrorData::invalid_params(format!("not found: {s}"), None),
        e @ Error::Parse { .. } => ErrorData::invalid_params(e.to_string(), None),
        e => ErrorData::internal_error(e.to_string(), None),
    }
}

fn node_types(v: &[String]) -> Result<Vec<NodeType>, ErrorData> {
    v.iter()
        .map(|s| NodeType::parse(s))
        .collect::<engram_core::Result<_>>()
        .map_err(map_err)
}

fn edge_types(v: &[String]) -> Result<Vec<EdgeType>, ErrorData> {
    v.iter()
        .map(|s| EdgeType::parse(s))
        .collect::<engram_core::Result<_>>()
        .map_err(map_err)
}

/// The natural durability for a node type when the caller doesn't specify one.
fn default_durability(t: NodeType) -> Durability {
    match t {
        NodeType::Principle | NodeType::Decision | NodeType::Caution | NodeType::Anchor => {
            Durability::Stable
        }
        NodeType::Problem | NodeType::Resolution | NodeType::Insight => Durability::Episodic,
        NodeType::Intent => Durability::Volatile,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_durability_matches_ontology() {
        assert_eq!(default_durability(NodeType::Decision), Durability::Stable);
        assert_eq!(default_durability(NodeType::Insight), Durability::Episodic);
        assert_eq!(default_durability(NodeType::Intent), Durability::Volatile);
    }

    #[test]
    fn type_parsing_rejects_garbage() {
        assert!(node_types(&["Decision".into()]).is_ok());
        assert!(node_types(&["Nope".into()]).is_err());
        assert!(edge_types(&["because".into()]).is_ok());
        assert!(edge_types(&["relates_to".into()]).is_err());
    }

    #[tokio::test]
    async fn add_note_and_search_via_tools() {
        use engram_core::{FakeEmbedder, Store};
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        );
        let server = Engram::new(engine);

        let res = server
            .add_note(Parameters(AddNoteArgs {
                node_type: "Decision".into(),
                title: "Adopt SQLite WAL".into(),
                body: Some("concurrent reads".into()),
                durability: None,
                session_id: None,
                code_refs: vec![],
            }))
            .await
            .unwrap();
        assert!(!res.is_error.unwrap_or(false));

        let hits = server
            .search(Parameters(SearchArgs {
                query: "sqlite".into(),
                types: vec![],
                limit: None,
            }))
            .await
            .unwrap();
        assert!(!hits.is_error.unwrap_or(false));
        // the serialized hit text should mention the node
        let text = format!("{:?}", hits.content);
        assert!(text.contains("Adopt SQLite WAL"));
    }
}

#[cfg(test)]
mod tool_tests {
    use super::*;
    use engram_core::{FakeEmbedder, Store};

    fn server() -> Engram {
        Engram::new(Engine::new(
            Store::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ))
    }

    fn text_of(res: &CallToolResult) -> String {
        format!("{:?}", res.content)
    }

    fn id_of(r: &CallToolResult) -> String {
        let t = text_of(r);
        let start = t.find("\\\"id\\\": \\\"").unwrap() + 10;
        t[start..].split("\\\"").next().unwrap().to_string()
    }

    fn note(title: &str) -> AddNoteArgs {
        AddNoteArgs {
            node_type: "Decision".into(),
            title: title.into(),
            body: Some("shared body".into()),
            durability: None,
            session_id: None,
            code_refs: vec![],
        }
    }

    #[tokio::test]
    async fn add_note_stamps_process_session_id_when_client_omits_it() {
        let s = server();
        let id = id_of(
            &s.add_note(Parameters(note("Adopt SQLite WAL")))
                .await
                .unwrap(),
        );
        let node = s.engine.lock().unwrap().get_node(&id).unwrap().unwrap();
        assert_eq!(node.session_id.as_deref(), Some(&*s.session_id));
        assert!(s.session_id.starts_with("mcp-"));
    }

    #[tokio::test]
    async fn add_note_short_circuits_duplicates() {
        let s = server();
        let first = s
            .add_note(Parameters(note("Adopt SQLite WAL")))
            .await
            .unwrap();
        assert!(text_of(&first).contains("\\\"created\\\": true"));

        let dupe = s
            .add_note(Parameters(note("Adopt SQLite WAL")))
            .await
            .unwrap();
        let text = text_of(&dupe);
        assert!(text.contains("\\\"created\\\": false"), "got: {text}");
        assert!(text.contains("matched"));
    }

    #[tokio::test]
    async fn get_node_returns_parent_and_child_hierarchy() {
        let s = server();
        // Decision -because-> Principle (parent); Insight -about-> Decision (child).
        let principle = id_of(
            &s.add_note(Parameters(AddNoteArgs {
                node_type: "Principle".into(),
                title: "local first".into(),
                body: None,
                durability: None,
                session_id: None,
                code_refs: vec![],
            }))
            .await
            .unwrap(),
        );
        let decision = id_of(
            &s.add_note(Parameters(note("store data in sqlite")))
                .await
                .unwrap(),
        );
        let insight = id_of(
            &s.add_note(Parameters(AddNoteArgs {
                node_type: "Insight".into(),
                title: "wal mode matters".into(),
                body: None,
                durability: None,
                session_id: None,
                code_refs: vec![],
            }))
            .await
            .unwrap(),
        );

        s.link(Parameters(LinkArgs {
            from: decision.clone(),
            to: principle.clone(),
            edge_type: "because".into(),
            note: None,
            confidence: None,
        }))
        .await
        .unwrap();
        s.link(Parameters(LinkArgs {
            from: insight.clone(),
            to: decision.clone(),
            edge_type: "about".into(),
            note: None,
            confidence: None,
        }))
        .await
        .unwrap();

        let res = s
            .get_node(Parameters(GetNodeArgs {
                id: decision.clone(),
                parents: Some(2),
                children: Some(2),
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(text.contains("parents"), "got: {text}");
        assert!(text.contains("local first"), "parent node inlined: {text}");
        assert!(text.contains("children"), "got: {text}");
        assert!(
            text.contains("wal mode matters"),
            "child node inlined: {text}"
        );
        assert!(
            text.contains("trust"),
            "hierarchy nodes carry trust: {text}"
        );
    }

    #[tokio::test]
    async fn brief_tool_returns_markdown() {
        let s = server();
        s.add_note(Parameters(note("Backend in Rust")))
            .await
            .unwrap();
        let res = s
            .brief(Parameters(BriefArgs { max_chars: None }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(text.contains("# Engram brief"));
        assert!(text.contains("Backend in Rust"));
    }

    #[tokio::test]
    async fn unlink_and_update_edge_roundtrip() {
        let s = server();
        let a = s
            .add_note(Parameters(note("first decision")))
            .await
            .unwrap();
        let b = s
            .add_note(Parameters(note(
                "second decision zzz qqq xyz totally different",
            )))
            .await
            .unwrap();
        let id_of = |r: &CallToolResult| {
            let t = text_of(r);
            let start = t.find("\\\"id\\\": \\\"").unwrap() + 10;
            t[start..].split("\\\"").next().unwrap().to_string()
        };
        let (ia, ib) = (id_of(&a), id_of(&b));

        let linked = s
            .link(Parameters(LinkArgs {
                from: ia,
                to: ib,
                edge_type: "conflicts-with".into(),
                note: None,
                confidence: None,
            }))
            .await
            .unwrap();
        let edge_id = id_of(&linked);

        let upd = s
            .update_edge(Parameters(UpdateEdgeArgs {
                id: edge_id.clone(),
                status: Some("resolved".into()),
                note: None,
                confidence: None,
            }))
            .await
            .unwrap();
        assert!(text_of(&upd).contains("\\\"ok\\\": true"));

        let gone = s
            .unlink(Parameters(IdArg {
                id: edge_id.clone(),
            }))
            .await
            .unwrap();
        assert!(text_of(&gone).contains("\\\"ok\\\": true"));
        assert!(s.unlink(Parameters(IdArg { id: edge_id })).await.is_err());
    }
}

#[cfg(test)]
mod suspect_tests {
    use super::*;
    use engram_core::{FakeEmbedder, Store};

    #[tokio::test]
    async fn brief_lists_suspects_and_resolve_judges_them() {
        let s = Engram::new(Engine::new(
            Store::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ));
        let mk = |t: &str, ty: &str| AddNoteArgs {
            node_type: ty.into(),
            title: t.into(),
            body: None,
            durability: None,
            session_id: None,
            code_refs: vec![],
        };
        s.add_note(Parameters(mk("cache invalidation via ttl", "Decision")))
            .await
            .unwrap();
        // Cross-type twin: dodges the duplicate short-circuit, lands as a suspect.
        s.add_note(Parameters(mk("cache invalidation via ttl", "Caution")))
            .await
            .unwrap();

        let listed = format!("{:?}", s.list_suspects().await.unwrap().content);
        assert!(listed.contains("suspects"), "got: {listed}");
        let brief = s
            .brief(Parameters(BriefArgs { max_chars: None }))
            .await
            .unwrap();
        let brief_text = format!("{:?}", brief.content);
        assert!(
            brief_text.contains("Suspected conflicts"),
            "got: {brief_text}"
        );

        let sid = s.engine.lock().unwrap().suspects().unwrap().remove(0).id;
        let resolved = s
            .resolve_suspect(Parameters(ResolveSuspectArgs {
                id: sid,
                verdict: "conflict".into(),
            }))
            .await
            .unwrap();
        let text = format!("{:?}", resolved.content);
        assert!(text.contains("conflicts-with"), "got: {text}");
    }
}
