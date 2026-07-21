//! stdio MCP server (`rmcp`) exposing the Engram graph to Claude. Thin wrapper
//! over `engram_core::Engine` implementing the Appendix A tool contracts. Note:
//! `delete_node` is deliberately absent — hard delete is user-only (PLAN §6B),
//! so Claude has no tool for it.

use std::sync::{Arc, Mutex};

use engram_core::{
    Durability, EdgePatch, EdgeStatus, EdgeType, Engine, Error, Hub, NewEdge, NewNode, Node,
    NodePatch, NodeStatus, NodeType, Source, SuspectVerdict, WriteOutcome, registry,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourceTemplatesResult, ListResourcesResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult, Resource,
    ResourceContents, ResourceTemplate, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

const INSTRUCTIONS: &str = "\
Engram is the project's durable reasoning/decision memory as an editable graph. \
Call `brief` at the start of a session for a compact digest of the canon \
(conflicts, open work, principles, decisions, cautions). Use `search` before \
non-trivial work — hits carry their 1-hop neighbors, conflicts and supersessions \
first. Capture every decision as it happens — a feature request usually hides \
one (library picked, shape chosen, tradeoff accepted) and it belongs in the \
graph even though nobody said \"remember this\". Every write's response is a \
verdict, not a receipt: `add_note` returns {matched, created:false} on a \
near-duplicate (merge via `update_node`), `warnings` when the text lands near \
contradicted or superseded knowledge, and `suspects` when it queued unjudged \
look-alike pairs — judge those immediately with `resolve_suspect` and tell \
the user when one is a genuine contradiction; that alert is the one exception \
to silent capture. \
Link nodes with sentence-shaped edges (e.g. a Decision `because` a Principle); \
repair a wrong link with `unlink` / `update_edge`. When the brief lists \
suspected conflicts, judge them early via `resolve_suspect` (conflict | \
replaces | dismiss) — the scan only finds candidates; you are the judge. \
Nodes carry computed \
`trust` (0..1) and `stale` (trust < 0.3 — verify before relying). Trust reads \
only deliberate acts: updates (confirmed_at) and approvals; retrieval never \
refreshes it — being findable proves nothing. Stable-durability knowledge \
holds its trust flat until a judged conflict demotes it (withdrawing the \
conflict withdraws the demotion; drift is review-only and never demotes); \
episodic/volatile knowledge decays with time. If a stale node is still true, \
say so with `update_node` — that is what restores trust. Pinned nodes \
(constant trust, set by the user in the pane) are marked PINNED in the brief; \
pinning and unpinning are user-only gestures, and a `replaces` verdict that \
would archive a pinned node is refused — surface it to the user instead. \
Nodes can carry free-form `tags` — how the user slices the graph (phases, \
concerns). Reuse the recent tags the brief lists before inventing new ones; \
an unknown tag is simply created. \
`check_claim` verifies a statement against the canon via the local NLI \
model ({supports, contradicts, silent}) — use it before acting on an \
assumption; its verdicts are hints, never judgments. \
For history questions: `timeline` walks a node's replaces chain (\"how did \
this decision evolve\"), `audit` pages the mutation journal (\"what changed, \
who wrote this\"). `list_drift` finds nodes whose code_refs no longer exist \
in the project — repair the refs via `update_node` and re-check the claim. \
For whole-graph work, use the bulk tools: `list_nodes` pages complete nodes \
(full bodies — the lossless read behind \"export every Decision to a \
decisions.md\"), `update_nodes` applies many patches in one call (curation \
sweeps), `add_notes` batch-creates with the same dupe checks as add_note. \
Most tools take an optional `project`: omit it for this project; a name/id \
(see `list_projects`) reads or writes THAT project's graph — capturing an \
insight about a sibling project into its own graph is deliberate and \
encouraged; `home` is the user-level graph for knowledge that transcends \
projects (global principles, preferences — write there on \"remember this \
globally\"); `search`/`check_claim` accept `project: \"all\"` to read across \
every graph (foreign hits carry provenance and a locality prior). Writes to \
`all` are refused — one insight lives in one graph, not N copies. \
Never store secrets or volatile implementation detail.";

/// Upper bound on items per batch tool call — big enough for any real
/// curation sweep, small enough to keep one call's audit burst readable.
const BATCH_CAP: usize = 100;

/// Attached to write responses that queued suspects: the write landed, but
/// the graph now holds an unjudged look-alike pair — the writer must close
/// that loop in the same turn, not leave it for the next session's brief.
const SUSPECT_ACTION: &str = "This note closely resembles existing unlinked knowledge (see `suspects`). \
Judge each pair NOW with resolve_suspect: `conflict` if they contradict (then tell the user — a live \
contradiction with standing canon is the one thing silent capture must surface), `replaces` if this \
write supersedes the older claim, `dismiss` if they are fine together.";

#[derive(Clone)]
pub struct Engram {
    /// The multi-project hub (PLAN §7C). Single-project constructions get a
    /// factory-less hub, so cross-project selectors fail with a clear message.
    hub: Arc<Hub>,
    /// The launch project's engine (== `hub.current()`), cached for the
    /// unscoped fast path.
    engine: Arc<Mutex<Engine>>,
    /// Fallback session id when the client omits one: minted once per server
    /// process, which over stdio is one Claude session. Superseded by the
    /// transport session id after the streamable-HTTP migration (PLAN §0).
    session_id: Arc<str>,
}

#[tool_router]
impl Engram {
    pub fn new(engine: Engine) -> Self {
        Self::with_hub(Arc::new(Hub::single(engine)))
    }

    /// Build over an engine shared with the HTTP server (same DB + listener).
    pub fn with_shared(engine: Arc<Mutex<Engine>>) -> Self {
        Self::with_hub(Arc::new(Hub::single_shared(engine)))
    }

    /// The full multi-project form: the same hub the HTTP server holds.
    pub fn with_hub(hub: Arc<Hub>) -> Self {
        Self {
            engine: hub.current_engine(),
            hub,
            session_id: format!("mcp-{}", engram_core::id::new_id()).into(),
        }
    }

    /// Bound to one project of the hub (v0.6.2 machine core): `selector`'s
    /// engine is this session's current project — what an omitted `project`
    /// param means — regardless of which project the core launched with.
    pub fn for_project(hub: Arc<Hub>, selector: &str) -> Result<Self, engram_core::Error> {
        Ok(Self {
            engine: hub.get(selector)?,
            hub,
            session_id: format!("mcp-{}", engram_core::id::new_id()).into(),
        })
    }

    /// Lock the engine and stamp this MCP session as the writer (audit journal
    /// attribution). Re-stamped on every operation: the engine may be shared
    /// with the HTTP pane, which stamps itself the same way.
    fn engine(&self) -> std::sync::MutexGuard<'_, Engine> {
        let mut guard = self.engine.lock().unwrap();
        guard.set_audit_origin(engram_core::AuditOrigin::mcp(self.session_id.to_string()));
        guard
    }

    /// Resolve a tool's optional `project` selector: omitted = the current
    /// project, a name/id = that registered project, `home` = the user-level
    /// home graph. `all` never resolves to one engine — the hub's refusal
    /// explains where fan-out reads and shared writes belong.
    fn engine_for(&self, project: &Option<String>) -> Result<Arc<Mutex<Engine>>, ErrorData> {
        match project.as_deref() {
            None => Ok(self.engine.clone()),
            Some(sel) => self.hub.get(sel).map_err(map_err),
        }
    }

    /// Lock a scoped engine with this session stamped as the writer.
    fn mcp<'a>(&self, engine: &'a Arc<Mutex<Engine>>) -> std::sync::MutexGuard<'a, Engine> {
        let mut guard = engine.lock().unwrap();
        guard.set_audit_origin(engram_core::AuditOrigin::mcp(self.session_id.to_string()));
        guard
    }

    #[tool(
        description = "Hybrid semantic + keyword search over the memory graph. \
        Hits carry: type, title, snippet, score, trust (computed 0..1), stale \
        (true = decayed trust, verify before relying), status, and 1-hop \
        neighbors (conflicts-with/replaces first). Being returned stamps \
        last_seen for observability only — retrieval never refreshes trust. \
        `project: \"all\"` searches every registered project plus the home \
        graph — foreign hits carry `project` provenance and rank under a \
        locality prior, so the local canon wins ties."
    )]
    async fn search(
        &self,
        Parameters(a): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let types = node_types(&a.types)?;
        let limit = a.limit.unwrap_or(8);
        if a.project.as_deref() == Some(registry::ALL_PROJECTS) {
            let (mut hits, skipped) = self
                .hub
                .search_all(&a.query, &types, limit)
                .map_err(map_err)?;
            hits.iter_mut().for_each(debracket);
            return ok_json(&json!({ "hits": hits, "skipped": skipped }));
        }
        let engine = self.engine_for(&a.project)?;
        let mut hits = self
            .mcp(&engine)
            .search(&a.query, &types, limit)
            .map_err(map_err)?;
        hits.iter_mut().for_each(debracket);
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
        let engine = self.engine_for(&a.project)?;
        let engine = self.mcp(&engine);
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
        let engine = self.engine_for(&a.project)?;
        let (nodes, edges) = self
            .mcp(&engine)
            .traverse(&a.from, &edge_types, a.depth.unwrap_or(2))
            .map_err(map_err)?;
        ok_json(&json!({ "nodes": nodes, "edges": edges }))
    }

    #[tool(
        description = "Create a memory node (source = claude, starts provisional). \
        ALWAYS read the response, it is a verdict, not a receipt — every check runs in this \
        same turn: {matched, created: false} = a same-type near-duplicate exists, merge via \
        update_node (if it carries nli_label=contradiction it is a NEGATED duplicate — read \
        before merging, likely a conflicts-with instead); `warnings` = the note landed near \
        contradicted or superseded knowledge; `missing_code_refs` = paths that don't resolve \
        in the repo, fix or drop them; `suspects` = queued look-alike pairs (each may carry \
        an nli hint), judge each with resolve_suspect now and tell the user if one is a \
        genuine contradiction."
    )]
    async fn add_note(
        &self,
        Parameters(a): Parameters<AddNoteArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let payload = self.create_note(a)?;
        ok_json(&payload)
    }

    /// The add_note core, shared with the batch form. Each note resolves its
    /// own `project` — a write addressed to `all` is refused by the hub with
    /// the home-graph pointer (PLAN §7C: fan-out writes are replication).
    fn create_note(&self, a: AddNoteArgs) -> Result<serde_json::Value, ErrorData> {
        let node_type = NodeType::parse(&a.node_type).map_err(map_err)?;
        let durability = match a.durability {
            Some(d) => Durability::parse(&d).map_err(map_err)?,
            None => default_durability(node_type),
        };
        let status = match node_type {
            NodeType::Problem | NodeType::Intent => Some(NodeStatus::Open),
            _ => None,
        };
        let engine = self.engine_for(&a.project)?;
        let outcome = self
            .mcp(&engine)
            .add_node_checked(NewNode {
                node_type,
                title: a.title,
                body: a.body,
                durability,
                source: Source::Claude,
                session_id: a.session_id.or_else(|| Some(self.session_id.to_string())),
                status,
                code_refs: a.code_refs,
                tags: a.tags,
            })
            .map_err(map_err)?;
        Ok(match outcome {
            WriteOutcome::Created {
                node,
                warnings,
                suspects,
                missing_refs,
            } => {
                let mut out = json!({ "id": node.id, "created": true });
                if !warnings.is_empty() {
                    out["warnings"] = json!(warnings);
                }
                if !missing_refs.is_empty() {
                    out["missing_code_refs"] = json!(missing_refs);
                    out["refs_note"] = json!(
                        "these code_refs don't resolve in the repo right now — fix the paths or drop them"
                    );
                }
                if !suspects.is_empty() {
                    out["suspects"] = json!(suspects);
                    out["action_required"] = json!(SUSPECT_ACTION);
                }
                out
            }
            WriteOutcome::Matched {
                node,
                similarity,
                nli_label,
                nli_score,
            } => {
                let mut out = json!({
                    "matched": node.id,
                    "created": false,
                    "title": node.title,
                    "similarity": similarity,
                });
                if let (Some(label), Some(score)) = (&nli_label, nli_score) {
                    out["nli_label"] = json!(label);
                    out["nli_score"] = json!(score);
                    if label == "contradiction" {
                        out["action_required"] = json!(
                            "The near-duplicate may CONTRADICT your text (negated duplicate — \
                             'use X' vs 'don't use X'). Read the matched node before merging; \
                             if it genuinely disagrees, capture yours as a new node and link \
                             conflicts-with instead of updating the match."
                        );
                    }
                }
                out
            }
        })
    }

    #[tool(
        description = "Batch create: add several notes in one call — each item \
        runs the same near-duplicate pre-check and redaction as add_note. \
        Results are per-item and positional: {id, created} | {matched, \
        created: false} | {ok: false, error}; one bad item never blocks the \
        rest. For seeding passes and multi-note stopping points."
    )]
    async fn add_notes(
        &self,
        Parameters(a): Parameters<AddNotesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if a.notes.len() > BATCH_CAP {
            return Err(ErrorData::invalid_params(
                format!("at most {BATCH_CAP} notes per call"),
                None,
            ));
        }
        let results: Vec<serde_json::Value> = a
            .notes
            .into_iter()
            .map(|item| {
                self.create_note(item)
                    .unwrap_or_else(|e| json!({ "ok": false, "error": e.message }))
            })
            .collect();
        ok_json(&json!({ "results": results }))
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
        let max_chars = a
            .max_chars
            .unwrap_or(engram_core::policy::DEFAULT_BRIEF_CHARS);
        // Unscoped = the current project plus the home-graph section; a
        // scoped project (or `home`) briefs that graph alone.
        let text = match &a.project {
            None => self.hub.brief(max_chars).map_err(map_err)?,
            Some(_) => {
                let engine = self.engine_for(&a.project)?;
                self.mcp(&engine).brief(max_chars).map_err(map_err)?
            }
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(description = "Delete one edge by id — for repairing a mislink. \
        Nodes are never deleted this way (hard node delete is user-only).")]
    async fn unlink(&self, Parameters(a): Parameters<IdArg>) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let removed = self.mcp(&engine).delete_edge(&a.id).map_err(map_err)?;
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
            // Retype stays a pane action; Claude repairs a wrong verb with
            // unlink + link (which re-states the sentence deliberately).
            edge_type: None,
            status: a
                .status
                .map(|s| EdgeStatus::parse(&s))
                .transpose()
                .map_err(map_err)?,
            note: a.note,
            confidence: a.confidence,
            strength: None,
        };
        let engine = self.engine_for(&a.project)?;
        let edge = self
            .mcp(&engine)
            .update_edge(&a.id, patch)
            .map_err(map_err)?;
        ok_json(&json!({ "ok": true, "id": edge.id }))
    }

    #[tool(description = "Link two nodes with a sentence-shaped edge \
        (about, because, answers, builds-on, replaces, conflicts-with, needs).")]
    async fn link(&self, Parameters(a): Parameters<LinkArgs>) -> Result<CallToolResult, ErrorData> {
        let edge_type = EdgeType::parse(&a.edge_type).map_err(map_err)?;
        let engine = self.engine_for(&a.project)?;
        let edge = self
            .mcp(&engine)
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
        description = "Verify a claim against the memory graph using the local \
        NLI model: returns {supports, contradicts, silent} — nodes that entail \
        the claim, nodes that contradict it, and nearby nodes with no verdict. \
        Use before acting on an assumption ('does the canon contradict this \
        plan?'). Contradicts-hits are conflicts to surface; all-silent on a \
        real topic is a gap worth capturing. Verdicts are hints from a small \
        local model — judgment stays with you."
    )]
    async fn check_claim(
        &self,
        Parameters(a): Parameters<CheckClaimArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = a.limit.unwrap_or(8);
        if a.project.as_deref() == Some(registry::ALL_PROJECTS) {
            let (report, skipped) = self.hub.check_claim_all(&a.claim, limit).map_err(map_err)?;
            let mut out = json!(report);
            out["skipped"] = json!(skipped);
            return ok_json(&out);
        }
        let engine = self.engine_for(&a.project)?;
        let report = self
            .mcp(&engine)
            .check_claim(&a.claim, limit)
            .map_err(map_err)?;
        ok_json(&report)
    }

    #[tool(
        description = "Pending suspected conflicts from the local scan: unlinked \
        look-alike node pairs awaiting judgment (each may carry an nli_label / \
        nli_score triage hint from the local model — a suggestion, not a \
        verdict). Judge each with resolve_suspect."
    )]
    async fn list_suspects(
        &self,
        Parameters(a): Parameters<ProjectArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let suspects = self.mcp(&engine).suspects().map_err(map_err)?;
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
        let engine = self.engine_for(&a.project)?;
        let edge = self
            .mcp(&engine)
            .resolve_suspect(&a.id, verdict, Source::Claude)
            .map_err(map_err)?;
        ok_json(&json!({ "ok": true, "edge": edge }))
    }

    #[tool(description = "Approve a node: trust restarts at 100% (and holds \
        there on stable knowledge until contradicting evidence lands). ONLY on \
        explicit user demand, or after verifying the node's content \
        word-by-word against current reality. Routine still-relevant signals \
        belong in update_node, not here.")]
    async fn approve_node(
        &self,
        Parameters(a): Parameters<ApproveArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let node = self.mcp(&engine).approve(&a.id).map_err(map_err)?;
        ok_json(&json!({ "ok": true, "id": node.id, "trust": node.trust }))
    }

    #[tool(
        description = "Update fields on an existing node (merge / reclassify / \
        confirm still true). A deliberate update stamps confirmed_at — the \
        unapproved trust anchor — and clears any evidence demotion; this is \
        how a verified-still-true stale node gets its trust back. Re-embeds \
        when any indexed field changes. Read the response like add_note's: \
        `warnings` and `suspects` carry the same act-now duties (judge \
        suspects via resolve_suspect; surface real contradictions to the user)."
    )]
    async fn update_node(
        &self,
        Parameters(a): Parameters<UpdateArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let payload = self.patch_node(a)?;
        ok_json(&payload)
    }

    /// The update_node core, shared with the batch form.
    fn patch_node(&self, a: UpdateArgs) -> Result<serde_json::Value, ErrorData> {
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
            tags: a.tags,
        };
        let engine = self.engine_for(&a.project)?;
        let engram_core::CheckedUpdate {
            node,
            warnings,
            suspects,
            missing_refs,
        } = self
            .mcp(&engine)
            .update_node_checked(&a.id, patch)
            .map_err(map_err)?;
        let mut out = json!({ "ok": true, "id": node.id });
        if !warnings.is_empty() {
            out["warnings"] = json!(warnings);
        }
        if !missing_refs.is_empty() {
            out["missing_code_refs"] = json!(missing_refs);
            out["refs_note"] = json!(
                "these code_refs don't resolve in the repo right now — fix the paths or drop them"
            );
        }
        if !suspects.is_empty() {
            out["suspects"] = json!(suspects);
            out["action_required"] = json!(SUSPECT_ACTION);
        }
        Ok(out)
    }

    #[tool(
        description = "Batch update: apply several node patches in one call — \
        the bulk counterpart of update_node for curation sweeps (term renames, \
        status fixes, tag hygiene). Each item takes the same fields as \
        update_node; items apply independently, results are positional \
        ({ok, id} | {ok: false, id, error}), one bad item never blocks the \
        rest, and every change lands in the audit journal individually."
    )]
    async fn update_nodes(
        &self,
        Parameters(a): Parameters<UpdateNodesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if a.updates.len() > BATCH_CAP {
            return Err(ErrorData::invalid_params(
                format!("at most {BATCH_CAP} updates per call"),
                None,
            ));
        }
        let results: Vec<serde_json::Value> = a
            .updates
            .into_iter()
            .map(|item| {
                let id = item.id.clone();
                self.patch_node(item)
                    .unwrap_or_else(|e| json!({ "ok": false, "id": id, "error": e.message }))
            })
            .collect();
        ok_json(&json!({ "results": results }))
    }

    #[tool(description = "Full-fidelity paged read of the graph: complete nodes \
        (whole body, tags, status, durability, code_refs, computed trust) with \
        optional filters — types, status, tag, include_archived, pinned \
        (pinned: true reads the user's constant-trust canon). This is the \
        lossless bulk read for reviews and exports: building a decisions.md \
        means paging every Decision with its full body, which search snippets \
        and the budgeted brief cannot provide. Newest first; `total` is the \
        filtered count, page with limit/offset. Read-only — does not refresh \
        trust clocks.")]
    async fn list_nodes(
        &self,
        Parameters(a): Parameters<ListNodesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let types = node_types(&a.types)?;
        let status = a
            .status
            .map(|s| NodeStatus::parse(&s))
            .transpose()
            .map_err(map_err)?;
        let tag = a
            .tag
            .map(|t| engram_core::normalize_tags(&[t]))
            .and_then(|mut v| v.pop());
        let engine = self.engine_for(&a.project)?;
        let (mut nodes, _) = self.mcp(&engine).graph().map_err(map_err)?;
        nodes.retain(|n| {
            (a.include_archived.unwrap_or(false) || n.valid_until.is_none())
                && (types.is_empty() || types.contains(&n.node_type))
                && status.is_none_or(|s| n.status == Some(s))
                && tag.as_ref().is_none_or(|t| n.tags.contains(t))
                && a.pinned.is_none_or(|p| n.trust_override.is_some() == p)
        });
        // Ids are time-sortable, so this is newest-first creation order.
        nodes.sort_by(|x, y| y.id.cmp(&x.id));
        let total = nodes.len();
        let offset = a.offset.unwrap_or(0);
        let limit = a.limit.unwrap_or(30).min(200);
        let page: Vec<Node> = nodes.into_iter().skip(offset).take(limit).collect();
        ok_json(&json!({ "total": total, "offset": offset, "nodes": page }))
    }

    #[tool(description = "List the live worklist: open Problems and Intents.")]
    async fn list_open(
        &self,
        Parameters(a): Parameters<ListOpenArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let types = node_types(&a.types)?;
        let engine = self.engine_for(&a.project)?;
        let nodes = self
            .mcp(&engine)
            .worklist(&types, a.include_conflicts.unwrap_or(true))
            .map_err(map_err)?;
        ok_json(&nodes)
    }

    #[tool(
        description = "The chronological story of one piece of knowledge: the \
        node's `replaces` chain, oldest first. Each superseded generation \
        carries the note of the replaces edge that retired it. Use to answer \
        \"how did this decision evolve\"."
    )]
    async fn timeline(
        &self,
        Parameters(a): Parameters<IdArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let chain = self.mcp(&engine).timeline(&a.id).map_err(map_err)?;
        ok_json(&json!({ "timeline": chain }))
    }

    #[tool(description = "Nodes whose path-shaped code_refs no longer exist in \
        the project — the code moved and the memory didn't (drifted). Review \
        each: fix the refs via update_node, and check whether the knowledge \
        itself is still true (supersede or conflicts-with it if not).")]
    async fn list_drift(
        &self,
        Parameters(a): Parameters<ProjectArg>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let engine = self.mcp(&engine);
        // A scoped project's refs resolve against *its* repo root; the cwd is
        // only the launch project's fallback.
        let root = match engine.repo_root() {
            Some(r) => r.to_path_buf(),
            None => std::env::current_dir()
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
        };
        let drifted = engine.scan_code_refs(&root).map_err(map_err)?;
        ok_json(&json!({ "drifted": drifted }))
    }

    #[tool(description = "One page of the audit journal, newest first: every \
        node/edge mutation with before/after snapshots and writer context \
        (origin, session, cwd, pid, version). Filter to one node/edge with \
        entity_id; page with before = the last row's seq. Read-only — answers \
        \"what changed while I was away\" and \"who wrote this\".")]
    async fn audit(
        &self,
        Parameters(a): Parameters<AuditArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine_for(&a.project)?;
        let page = self
            .mcp(&engine)
            .audit_log(
                a.before,
                a.entity_id.as_deref(),
                a.limit.unwrap_or(20).min(200),
            )
            .map_err(map_err)?;
        ok_json(&page)
    }

    #[tool(description = "Every project this memory hub can reach: the current \
        project, the user-level home graph, and the machine registry \
        (~/.engram/registry.json — populated by every engram-alpha serve/mcp \
        run). Use the names here as the `project` argument other tools accept: \
        omit = current, a name = that project (reads AND writes), 'home' = \
        the shared user-level graph, 'all' = fan a search/check_claim out \
        across everything (reads only).")]
    async fn list_projects(&self) -> Result<CallToolResult, ErrorData> {
        ok_json(&json!({ "projects": self.hub.projects() }))
    }
}

/// How many concrete node resources `resources/list` advertises (newest
/// first); the full graph stays reachable through the uri template.
const RESOURCE_LIST_CAP: usize = 25;

#[tool_handler]
impl ServerHandler for Engram {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("engram", env!("CARGO_PKG_VERSION")))
        .with_instructions(INSTRUCTIONS.to_string())
    }

    /// Appendix A: `engram://node/{id}` so a user can @-mention a node in a
    /// prompt. The list shows the newest nodes; anything else resolves
    /// through the template with an id from `search`/the pane.
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let nodes = self
            .engine()
            .store()
            .recent_nodes(RESOURCE_LIST_CAP)
            .map_err(map_err)?;
        Ok(ListResourcesResult {
            meta: None,
            next_cursor: None,
            resources: nodes
                .into_iter()
                .map(|n| {
                    Resource::new(format!("engram://node/{}", n.id), n.id.clone())
                        .with_title(n.title)
                        .with_description(format!(
                            "{} node in the Engram memory graph",
                            n.node_type.as_str()
                        ))
                        .with_mime_type("application/json")
                })
                .collect(),
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult {
            meta: None,
            next_cursor: None,
            resource_templates: vec![
                ResourceTemplate::new("engram://node/{id}", "node")
                    .with_title("Engram memory node")
                    .with_description(
                        "One memory node with its edges, by id (ids come from search, \
                         the brief, or the pane)",
                    )
                    .with_mime_type("application/json"),
            ],
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let Some(id) = request.uri.strip_prefix("engram://node/") else {
            return Err(ErrorData::invalid_params(
                format!("unknown resource uri: {}", request.uri),
                None,
            ));
        };
        let engine = self.engine();
        let Some(node) = engine.get_node(id).map_err(map_err)? else {
            return Err(ErrorData::invalid_params(
                format!("node not found: {id}"),
                None,
            ));
        };
        let out = engine.edges_out(id).map_err(map_err)?;
        let incoming = engine.edges_in(id).map_err(map_err)?;
        let payload = json!({ "node": node, "edges_out": out, "edges_in": incoming });
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(payload.to_string(), request.uri)
                .with_mime_type("application/json"),
        ]))
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

/// Serve over stdio with the full multi-project hub (PLAN §7C) — the hub the
/// daemon's HTTP server shares, or a standalone one for `engram-alpha mcp`.
pub async fn serve_stdio_hub(hub: Arc<Hub>) -> anyhow::Result<()> {
    serve(Engram::with_hub(hub)).await
}

async fn serve(server: Engram) -> anyhow::Result<()> {
    let service = server.serve(rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

// ---- daemon-hosted MCP (PLAN §0 transport migration / §7C thin clients) ----

/// The daemon-hosted MCP service type, nameable so the CLI can cache one per
/// project (v0.6.2: `/projects/{id}/mcp`).
pub type McpHttpService = rmcp::transport::StreamableHttpService<
    Engram,
    rmcp::transport::streamable_http_server::session::local::LocalSessionManager,
>;

/// The daemon's `/mcp` endpoint: MCP over streamable HTTP as a tower service
/// for the daemon router. Stateful — each connected client becomes one
/// session with its own [`Engram`] instance over the shared hub, so
/// per-session audit attribution works exactly as one stdio process did.
pub fn streamable_http_service(hub: Arc<Hub>) -> McpHttpService {
    rmcp::transport::StreamableHttpService::new(
        move || Ok(Engram::with_hub(hub.clone())),
        Arc::new(Default::default()),
        Default::default(),
    )
}

/// The per-project form (v0.6.2 machine core): sessions on this service
/// treat `selector`'s graph as the current project — an MCP bridge from repo
/// X binds to X, however the core was launched. The `project` tool param
/// still overrides per call.
pub fn streamable_http_service_for(hub: Arc<Hub>, selector: String) -> McpHttpService {
    rmcp::transport::StreamableHttpService::new(
        move || {
            Engram::for_project(hub.clone(), &selector)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()))
        },
        Arc::new(Default::default()),
        Default::default(),
    )
}

/// The stdio side of the thin client: a verbatim MCP passthrough from a stdio
/// client (Claude Code and friends launch us this way) to the daemon's `/mcp`
/// endpoint. Exists because redb allows one process per store — the daemon
/// holds the file; everything else, including this bridge, talks HTTP.
struct Passthrough {
    upstream: rmcp::service::Peer<rmcp::RoleClient>,
    info: rmcp::model::ServerInfo,
}

fn proxy_err(e: rmcp::ServiceError) -> ErrorData {
    match e {
        rmcp::ServiceError::McpError(data) => data,
        other => ErrorData::internal_error(format!("daemon bridge: {other}"), None),
    }
}

impl rmcp::Service<rmcp::RoleServer> for Passthrough {
    async fn handle_request(
        &self,
        request: rmcp::model::ClientRequest,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ServerResult, ErrorData> {
        self.upstream.send_request(request).await.map_err(proxy_err)
    }

    async fn handle_notification(
        &self,
        notification: rmcp::model::ClientNotification,
        _context: rmcp::service::NotificationContext<rmcp::RoleServer>,
    ) -> Result<(), ErrorData> {
        self.upstream
            .send_notification(notification)
            .await
            .map_err(proxy_err)
    }

    fn get_info(&self) -> rmcp::model::ServerInfo {
        // Mirror what the daemon negotiated, so the stdio client sees the
        // real server — same name, version, instructions, capabilities.
        self.info.clone()
    }
}

/// Serve stdio by bridging every message to the daemon's MCP endpoint
/// (`http://127.0.0.1:<port>/mcp`) until the stdio client disconnects.
pub async fn serve_stdio_proxy(url: &str) -> anyhow::Result<()> {
    let transport = rmcp::transport::StreamableHttpClientTransport::from_uri(url.to_string());
    let client = ().serve(transport).await?;
    let info = client
        .peer()
        .peer_info()
        .map(|i| (*i).clone())
        .ok_or_else(|| anyhow::anyhow!("daemon MCP handshake returned no server info"))?;
    let proxy = Passthrough {
        upstream: client.peer().clone(),
        info,
    };
    let service = proxy.serve(rmcp::transport::io::stdio()).await?;
    // Satellites die with the core (v0.6.2): whichever side ends first ends
    // the bridge — stdio closing is a normal disconnect, the upstream dying
    // means the core is gone and lingering would only strand the client.
    // Satellites die with the core (v0.6.2). The HTTP client transport
    // auto-reconnects, so a dead core never surfaces as a closed connection —
    // liveness has to be probed: ping the core on a heartbeat and treat a
    // timed-out or failed ping as its death. Both sides' futures own their
    // service; the bridge process ends right after the select, which tears
    // the loser down with it.
    let heartbeat_peer = client.peer().clone();
    let core_died = async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            let ping = heartbeat_peer.send_request(rmcp::model::ClientRequest::PingRequest(
                rmcp::model::PingRequest {
                    method: Default::default(),
                    extensions: Default::default(),
                },
            ));
            match tokio::time::timeout(std::time::Duration::from_secs(10), ping).await {
                Ok(Ok(_)) => {}
                _ => return,
            }
        }
    };
    tokio::select! {
        served = service.waiting() => {
            served?;
            Ok(())
        }
        _ = core_died => {
            anyhow::bail!("the engram core went away — bridge exiting")
        }
    }
}

// ---- argument schemas ---------------------------------------------------
//
// Every scoped tool takes the same optional `project` selector (PLAN §7C):
// omitted = the current project; a registered name/id = that project (reads
// AND writes — capturing into a sibling repo's graph is deliberate); "home" =
// the user-level home graph; "all" = every project, reads only (search /
// check_claim). `list_projects` names what exists.

#[derive(Deserialize, JsonSchema)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    types: Vec<String>,
    #[serde(default)]
    limit: Option<usize>,
    /// Omit = current project; name/id = that project; "home"; "all" =
    /// every project + home with provenance (reads only).
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct IdArg {
    id: String,
    /// Omit = current project; name/id = that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ProjectArg {
    /// Omit = current project; name/id = that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct AuditArgs {
    /// Max rows to return (default 20, newest first).
    #[serde(default)]
    limit: Option<usize>,
    /// Keyset cursor: only rows with seq strictly below this (page with the
    /// last row's seq).
    #[serde(default)]
    before: Option<i64>,
    /// Restrict to one node/edge id's history.
    #[serde(default)]
    entity_id: Option<String>,
    /// Omit = current project; name/id = that project; "home".
    #[serde(default)]
    project: Option<String>,
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
    /// Omit = current project; name/id = that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct TraverseArgs {
    from: String,
    #[serde(default)]
    edge_types: Vec<String>,
    #[serde(default)]
    depth: Option<usize>,
    /// Omit = current project; name/id = that project; "home".
    #[serde(default)]
    project: Option<String>,
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
    /// Free-form slice labels (kebab-cased on write). Reuse the recent tags
    /// listed in the brief before inventing new ones; new tags are created
    /// implicitly.
    #[serde(default)]
    tags: Vec<String>,
    /// Omit = current project; a name/id writes into THAT project's graph
    /// (deliberate cross-project capture); "home" = the user-level graph.
    /// "all" is refused — a fanned-out write is replication.
    #[serde(default)]
    project: Option<String>,
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
    /// Omit = current project; name/id = that project (both endpoints must
    /// live there — edges never cross graphs); "home".
    #[serde(default)]
    project: Option<String>,
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
    /// Replaces the node's tag list when set (kebab-cased on write).
    #[serde(default)]
    tags: Option<Vec<String>>,
    /// Omit = current project; name/id = that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct AddNotesArgs {
    /// Notes to create; each item takes the same fields as add_note.
    notes: Vec<AddNoteArgs>,
}

#[derive(Deserialize, JsonSchema)]
struct UpdateNodesArgs {
    /// Patches to apply; each item takes the same fields as update_node.
    updates: Vec<UpdateArgs>,
}

#[derive(Deserialize, JsonSchema)]
struct ListNodesArgs {
    /// Filter to these node types (default: all 8).
    #[serde(default)]
    types: Vec<String>,
    /// Filter Problems/Intents by status: open | resolved | obsolete.
    #[serde(default)]
    status: Option<String>,
    /// Only nodes carrying this tag.
    #[serde(default)]
    tag: Option<String>,
    /// Also return archived (superseded) generations. Default false.
    #[serde(default)]
    include_archived: Option<bool>,
    /// true = only user-pinned (constant-trust) nodes; false = only unpinned.
    #[serde(default)]
    pinned: Option<bool>,
    /// Page size (default 30, max 200).
    #[serde(default)]
    limit: Option<usize>,
    /// Skip this many (after filtering, newest first).
    #[serde(default)]
    offset: Option<usize>,
    /// Omit = current project; name/id = that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ApproveArgs {
    id: String,
    /// Omit = current project; name/id = that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct CheckClaimArgs {
    /// The claim to verify, as one declarative sentence.
    claim: String,
    /// How many nearby nodes to judge (default 8, max 16).
    #[serde(default)]
    limit: Option<usize>,
    /// Omit = current project; name/id = that project; "home"; "all" =
    /// judge across every project + home with provenance.
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ResolveSuspectArgs {
    /// The suspect id (from the brief's "Suspected conflicts" section or list_suspects).
    id: String,
    /// "conflict" | "replaces" | "dismiss"
    verdict: String,
    /// Omit = current project; name/id = that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ListOpenArgs {
    #[serde(default)]
    types: Vec<String>,
    #[serde(default)]
    include_conflicts: Option<bool>,
    /// Omit = current project; name/id = that project; "home".
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct BriefArgs {
    /// Character budget for the digest (default ~16000, about 4k tokens).
    #[serde(default)]
    max_chars: Option<usize>,
    /// Omit = current project's brief plus the home-graph section; a name/id
    /// (or "home") briefs that graph alone.
    #[serde(default)]
    project: Option<String>,
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
    /// Omit = current project; name/id = that project; "home".
    #[serde(default)]
    project: Option<String>,
}

// ---- helpers ------------------------------------------------------------

/// The store marks matches with private-use sentinels (the pane's highlight
/// markers); assistants read plain brackets instead.
fn debracket(hit: &mut engram_core::SearchHit) {
    hit.snippet = hit
        .snippet
        .replace(engram_core::SNIPPET_OPEN, "[")
        .replace(engram_core::SNIPPET_CLOSE, "]");
}

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
        e @ (Error::Parse { .. } | Error::Pinned(_) | Error::Project(_)) => {
            ErrorData::invalid_params(e.to_string(), None)
        }
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
        use engram_core::{FakeEmbedder, SqliteStore};
        let engine = Engine::new(
            SqliteStore::open_in_memory().unwrap(),
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
                tags: vec![],
                project: None,
            }))
            .await
            .unwrap();
        assert!(!res.is_error.unwrap_or(false));

        let hits = server
            .search(Parameters(SearchArgs {
                query: "sqlite".into(),
                types: vec![],
                limit: None,
                project: None,
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
    use engram_core::{FakeEmbedder, SqliteStore};

    fn server() -> Engram {
        Engram::new(Engine::new(
            SqliteStore::open_in_memory().unwrap(),
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
            tags: vec![],
            project: None,
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
    async fn add_note_persists_normalized_tags() {
        let s = server();
        let id = id_of(
            &s.add_note(Parameters(AddNoteArgs {
                tags: vec!["Phase 1".into(), "UI".into()],
                ..note("tagged decision")
            }))
            .await
            .unwrap(),
        );
        let node = s.engine.lock().unwrap().get_node(&id).unwrap().unwrap();
        assert_eq!(node.tags, vec!["phase-1", "ui"]);
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
                tags: vec![],
                project: None,
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
                tags: vec![],
                project: None,
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
            project: None,
        }))
        .await
        .unwrap();
        s.link(Parameters(LinkArgs {
            from: insight.clone(),
            to: decision.clone(),
            edge_type: "about".into(),
            note: None,
            confidence: None,
            project: None,
        }))
        .await
        .unwrap();

        let res = s
            .get_node(Parameters(GetNodeArgs {
                id: decision.clone(),
                parents: Some(2),
                children: Some(2),
                project: None,
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
            .brief(Parameters(BriefArgs {
                max_chars: None,
                project: None,
            }))
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
                project: None,
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
                project: None,
            }))
            .await
            .unwrap();
        assert!(text_of(&upd).contains("\\\"ok\\\": true"));

        let gone = s
            .unlink(Parameters(IdArg {
                id: edge_id.clone(),
                project: None,
            }))
            .await
            .unwrap();
        assert!(text_of(&gone).contains("\\\"ok\\\": true"));
        assert!(
            s.unlink(Parameters(IdArg {
                id: edge_id,
                project: None,
            }))
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn timeline_tool_orders_the_replaces_chain() {
        let s = server();
        let old = id_of(
            &s.add_note(Parameters(AddNoteArgs {
                body: Some("cookie sessions".into()),
                ..note("auth v1")
            }))
            .await
            .unwrap(),
        );
        let new = id_of(
            &s.add_note(Parameters(AddNoteArgs {
                body: Some("oauth device flow".into()),
                ..note("auth v2")
            }))
            .await
            .unwrap(),
        );
        s.link(Parameters(LinkArgs {
            from: new.clone(),
            to: old.clone(),
            edge_type: "replaces".into(),
            note: Some("cookies broke on mobile".into()),
            confidence: None,
            project: None,
        }))
        .await
        .unwrap();

        let t = text_of(
            &s.timeline(Parameters(IdArg {
                id: new,
                project: None,
            }))
            .await
            .unwrap(),
        );
        let (v1, v2) = (t.find("auth v1").unwrap(), t.find("auth v2").unwrap());
        assert!(v1 < v2, "oldest first: {t}");
        assert!(t.contains("cookies broke on mobile"));
        assert!(
            s.timeline(Parameters(IdArg {
                id: "nope".into(),
                project: None,
            }))
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn list_drift_flags_missing_refs() {
        let s = server();
        s.add_note(Parameters(AddNoteArgs {
            code_refs: vec!["Cargo.toml".into(), "src/vanished.rs".into()],
            ..note("refs moved")
        }))
        .await
        .unwrap();
        let t = text_of(
            &s.list_drift(Parameters(ProjectArg { project: None }))
                .await
                .unwrap(),
        );
        assert!(t.contains("src/vanished.rs"), "got: {t}");
        assert!(
            !t.contains("Cargo.toml"),
            "existing refs are not drift: {t}"
        );
    }

    #[tokio::test]
    async fn audit_tool_pages_the_journal() {
        let s = server();
        let id = id_of(&s.add_note(Parameters(note("journaled"))).await.unwrap());
        let t = text_of(
            &s.audit(Parameters(AuditArgs {
                limit: None,
                before: None,
                entity_id: Some(id),
                project: None,
            }))
            .await
            .unwrap(),
        );
        assert!(t.contains("created"), "got: {t}");
        assert!(t.contains("journaled"));
    }

    #[tokio::test]
    async fn bulk_create_read_update_roundtrip() {
        let s = server();
        // Batch create: a Decision with a long body, a tagged Caution, and a
        // near-duplicate of the first — the dupe check must run per item.
        let created = s
            .add_notes(Parameters(AddNotesArgs {
                notes: vec![
                    AddNoteArgs {
                        body: Some("the full body that an export must not lose".into()),
                        ..note("store data in sqlite")
                    },
                    AddNoteArgs {
                        node_type: "Caution".into(),
                        tags: vec!["hygiene".into()],
                        ..note("never trust a relative db path")
                    },
                    AddNoteArgs {
                        body: Some("the full body that an export must not lose".into()),
                        ..note("store data in sqlite")
                    },
                ],
            }))
            .await
            .unwrap();
        let text = text_of(&created);
        assert!(text.contains("created"), "got: {text}");
        assert!(text.contains("matched"), "per-item dupe check: {text}");

        // Full-fidelity filtered read: only Decisions, whole body included.
        let listed = s
            .list_nodes(Parameters(ListNodesArgs {
                types: vec!["Decision".into()],
                status: None,
                tag: None,
                include_archived: None,
                pinned: None,
                limit: None,
                offset: None,
                project: None,
            }))
            .await
            .unwrap();
        let text = text_of(&listed);
        assert!(
            text.contains("the full body that an export must not lose"),
            "full body survives the bulk read: {text}"
        );
        assert!(
            !text.contains("relative db path"),
            "type filter holds: {text}"
        );

        // Tag filter reaches the Caution.
        let tagged = s
            .list_nodes(Parameters(ListNodesArgs {
                types: vec![],
                status: None,
                tag: Some("hygiene".into()),
                include_archived: None,
                pinned: None,
                limit: None,
                offset: None,
                project: None,
            }))
            .await
            .unwrap();
        let text = text_of(&tagged);
        assert!(text.contains("relative db path"), "got: {text}");
        assert!(!text.contains("sqlite"), "got: {text}");
    }

    #[tokio::test]
    async fn update_nodes_applies_independently_and_reports_per_item() {
        let s = server();
        let id = id_of(
            &s.add_note(Parameters(note("original title")))
                .await
                .unwrap(),
        );
        let blank = |id: String| UpdateArgs {
            id,
            node_type: None,
            title: None,
            body: None,
            durability: None,
            status: None,
            code_refs: None,
            tags: None,
            project: None,
        };
        let res = s
            .update_nodes(Parameters(UpdateNodesArgs {
                updates: vec![
                    UpdateArgs {
                        title: Some("renamed title".into()),
                        ..blank(id.clone())
                    },
                    blank("nonexistent-id".into()),
                ],
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(
            text.contains("renamed title") || text.contains("true"),
            "got: {text}"
        );
        assert!(text.contains("false"), "bad id reported, not fatal: {text}");

        let node = s
            .get_node(Parameters(GetNodeArgs {
                id,
                parents: None,
                children: None,
                project: None,
            }))
            .await
            .unwrap();
        assert!(text_of(&node).contains("renamed title"), "patch landed");
    }

    #[tokio::test]
    async fn search_snippets_use_brackets_not_sentinels() {
        let s = server();
        s.add_note(Parameters(note("sentinel roundtrip check")))
            .await
            .unwrap();
        let res = s
            .search(Parameters(SearchArgs {
                query: "sentinel".into(),
                types: vec![],
                limit: None,
                project: None,
            }))
            .await
            .unwrap();
        let text = text_of(&res);
        assert!(text.contains('['), "brackets for assistants: {text}");
        assert!(
            !text.contains('\u{e000}') && !text.contains('\u{e001}'),
            "no raw sentinels leak over MCP: {text}"
        );
    }
}

#[cfg(test)]
mod project_tests {
    use super::*;
    use engram_core::{FakeEmbedder, SqliteStore};

    fn server() -> Engram {
        Engram::new(Engine::new(
            SqliteStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ))
    }

    #[tokio::test]
    async fn all_writes_are_refused_with_the_home_pointer() {
        let s = server();
        let err = s
            .add_note(Parameters(AddNoteArgs {
                node_type: "Decision".into(),
                title: "fan out".into(),
                body: None,
                durability: None,
                session_id: None,
                code_refs: vec![],
                tags: vec![],
                project: Some("all".into()),
            }))
            .await
            .unwrap_err();
        assert!(err.message.contains("home"), "got: {}", err.message);
    }

    #[tokio::test]
    async fn unknown_project_selector_is_invalid_params() {
        let s = server();
        let err = s
            .search(Parameters(SearchArgs {
                query: "anything".into(),
                types: vec![],
                limit: None,
                project: Some("definitely-not-registered-xyz".into()),
            }))
            .await
            .unwrap_err();
        assert!(
            err.message.contains("definitely-not-registered-xyz"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn list_projects_reports_the_current_project() {
        let s = server();
        let t = format!("{:?}", s.list_projects().await.unwrap().content);
        assert!(t.contains("projects"), "got: {t}");
        assert!(t.contains("current"), "got: {t}");
    }
}

#[cfg(test)]
mod suspect_tests {
    use super::*;
    use engram_core::{FakeEmbedder, SqliteStore};

    #[tokio::test]
    async fn brief_lists_suspects_and_resolve_judges_them() {
        let s = Engram::new(Engine::new(
            SqliteStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ));
        let mk = |t: &str, ty: &str| AddNoteArgs {
            node_type: ty.into(),
            title: t.into(),
            body: None,
            durability: None,
            session_id: None,
            code_refs: vec![],
            tags: vec![],
            project: None,
        };
        s.add_note(Parameters(mk("cache invalidation via ttl", "Decision")))
            .await
            .unwrap();
        // Cross-type twin: dodges the duplicate short-circuit, lands as a suspect.
        s.add_note(Parameters(mk("cache invalidation via ttl", "Caution")))
            .await
            .unwrap();

        let listed = format!(
            "{:?}",
            s.list_suspects(Parameters(ProjectArg { project: None }))
                .await
                .unwrap()
                .content
        );
        assert!(listed.contains("suspects"), "got: {listed}");
        let brief = s
            .brief(Parameters(BriefArgs {
                max_chars: None,
                project: None,
            }))
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
                project: None,
            }))
            .await
            .unwrap();
        let text = format!("{:?}", resolved.content);
        assert!(text.contains("conflicts-with"), "got: {text}");
    }

    #[tokio::test]
    async fn write_response_surfaces_freshly_queued_suspects() {
        let s = Engram::new(Engine::new(
            SqliteStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        ));
        let mk = |t: &str, ty: &str| AddNoteArgs {
            node_type: ty.into(),
            title: t.into(),
            body: None,
            durability: None,
            session_id: None,
            code_refs: vec![],
            tags: vec![],
            project: None,
        };
        let first = s
            .add_note(Parameters(mk(
                "retry queue drains on reconnect",
                "Decision",
            )))
            .await
            .unwrap();
        let first_text = format!("{:?}", first.content);
        assert!(
            !first_text.contains("suspects"),
            "nothing to suspect yet: {first_text}"
        );

        // Cross-type twin: dodges the duplicate short-circuit, queues a
        // suspect — which the WRITE RESPONSE itself must now surface.
        let second = s
            .add_note(Parameters(mk("retry queue drains on reconnect", "Caution")))
            .await
            .unwrap();
        let text = format!("{:?}", second.content);
        assert!(text.contains("suspects"), "got: {text}");
        assert!(
            text.contains("action_required") && text.contains("resolve_suspect"),
            "the response tells the writer what to do: {text}"
        );
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use engram_core::{FakeEmbedder, SqliteStore};
    use rmcp::model::CallToolRequestParams;

    /// The §7C thin-client chain, end to end and in-process: a daemon-style
    /// axum server hosting /mcp, a direct streamable-HTTP client against it,
    /// and a full stdio-shaped bridge (Passthrough over an in-memory duplex)
    /// relaying a second client through it.
    #[tokio::test]
    async fn streamable_http_daemon_and_stdio_bridge_end_to_end() {
        let engine = Engine::new(
            SqliteStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        );
        let hub = Arc::new(Hub::single(engine));
        let app = axum::Router::new().route_service("/mcp", streamable_http_service(hub.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let url = format!("http://{addr}/mcp");

        // Direct client: handshake + a real tool call against the endpoint.
        let direct = ()
            .serve(rmcp::transport::StreamableHttpClientTransport::from_uri(
                url.clone(),
            ))
            .await
            .unwrap();
        let tools = direct.peer().list_all_tools().await.unwrap();
        assert!(tools.iter().any(|t| t.name == "brief"));
        assert!(tools.iter().any(|t| t.name == "add_note"));
        let noted = direct
            .peer()
            .call_tool(
                CallToolRequestParams::new("add_note").with_arguments(
                    serde_json::json!({
                        "type": "Decision",
                        "title": "served over the daemon transport",
                        "durability": "stable"
                    })
                    .as_object()
                    .cloned()
                    .unwrap(),
                ),
            )
            .await
            .unwrap();
        assert_ne!(noted.is_error, Some(true));

        // The bridge: stdio-shaped duplex → Passthrough → the same endpoint.
        let upstream = ()
            .serve(rmcp::transport::StreamableHttpClientTransport::from_uri(
                url,
            ))
            .await
            .unwrap();
        let info = upstream.peer().peer_info().map(|i| (*i).clone()).unwrap();
        let proxy = Passthrough {
            upstream: upstream.peer().clone(),
            info,
        };
        let (bridge_io, client_io) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            let server = proxy.serve(bridge_io).await.unwrap();
            let _ = server.waiting().await;
        });
        let bridged = ().serve(client_io).await.unwrap();
        assert_eq!(
            bridged.peer().peer_info().unwrap().server_info.name,
            "engram",
            "the bridge mirrors the daemon's identity"
        );
        let hits = bridged
            .peer()
            .call_tool(
                CallToolRequestParams::new("search").with_arguments(
                    serde_json::json!({ "query": "daemon transport" })
                        .as_object()
                        .cloned()
                        .unwrap(),
                ),
            )
            .await
            .unwrap();
        assert_ne!(hits.is_error, Some(true));
        let text = format!("{:?}", hits.content);
        assert!(
            text.contains("served over the daemon transport"),
            "a write through the direct client is visible through the bridge: {text}"
        );
    }
}

#[cfg(test)]
mod scoped_transport_tests {
    use super::*;
    use engram_core::{FakeEmbedder, SqliteStore, registry};
    use rmcp::model::CallToolRequestParams;

    /// v0.6.2: a session on /projects/{id}/mcp treats that project as current
    /// — the repo-bound AI side of "one core, one pane".
    #[tokio::test]
    async fn scoped_mcp_endpoint_binds_sessions_to_their_project() {
        let tmp = std::env::temp_dir().join(format!("engram-mcp-scope-{}", std::process::id()));
        let beta_root = tmp.join("beta");
        std::fs::create_dir_all(beta_root.join(".engram")).unwrap();
        unsafe { std::env::set_var("ENGRAM_HOME", tmp.join("home")) };
        let beta_db = beta_root.join(".engram/graph.db");
        registry::register(&beta_root, &beta_db).unwrap();
        {
            let beta = Engine::new(
                SqliteStore::open(&beta_db).unwrap(),
                Box::new(FakeEmbedder::default()),
            );
            beta.add_node(engram_core::NewNode {
                node_type: engram_core::NodeType::Decision,
                title: "beta owns this decision".into(),
                body: None,
                durability: engram_core::Durability::Stable,
                source: engram_core::Source::Claude,
                session_id: None,
                status: None,
                code_refs: vec![],
                tags: vec![],
            })
            .unwrap();
        }

        let alpha = Engine::new(
            SqliteStore::open_in_memory().unwrap(),
            Box::new(FakeEmbedder::default()),
        );
        let factory: engram_core::EngineFactory = Box::new(|db| {
            Ok(Engine::new(
                SqliteStore::open(db)?,
                Box::new(FakeEmbedder::default()),
            ))
        });
        let hub = Arc::new(Hub::new(Arc::new(Mutex::new(alpha)), None, Some(factory)));

        let app = axum::Router::new()
            .route_service("/mcp", streamable_http_service(hub.clone()))
            .route_service(
                "/projects/beta/mcp",
                streamable_http_service_for(hub.clone(), "beta".into()),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let scoped = ()
            .serve(rmcp::transport::StreamableHttpClientTransport::from_uri(
                format!("http://{addr}/projects/beta/mcp"),
            ))
            .await
            .unwrap();
        let hits = scoped
            .peer()
            .call_tool(
                CallToolRequestParams::new("search").with_arguments(
                    serde_json::json!({ "query": "beta owns decision" })
                        .as_object()
                        .cloned()
                        .unwrap(),
                ),
            )
            .await
            .unwrap();
        let text = format!("{:?}", hits.content);
        assert!(
            text.contains("beta owns this decision"),
            "scoped session searches ITS project by default: {text}"
        );

        let unscoped = ()
            .serve(rmcp::transport::StreamableHttpClientTransport::from_uri(
                format!("http://{addr}/mcp"),
            ))
            .await
            .unwrap();
        let hits = unscoped
            .peer()
            .call_tool(
                CallToolRequestParams::new("search").with_arguments(
                    serde_json::json!({ "query": "beta owns decision" })
                        .as_object()
                        .cloned()
                        .unwrap(),
                ),
            )
            .await
            .unwrap();
        let text = format!("{:?}", hits.content);
        assert!(
            !text.contains("beta owns this decision"),
            "the hub's current project stays its own graph: {text}"
        );

        unsafe { std::env::remove_var("ENGRAM_HOME") };
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
