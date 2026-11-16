//! stdio MCP server (`rmcp`) exposing the Engram graph to Claude. Thin wrapper
//! over `engram_core::Engine` implementing the Appendix A tool contracts. Note:
//! `delete_node` is deliberately absent — hard delete is user-only,
//! so Claude has no tool for it.

mod args;
mod bridge;
mod format;
mod http;
mod server;
mod session;
#[cfg(test)]
mod tests;
mod tools;

use args::*;
pub use bridge::*;
use format::*;
pub use http::*;
pub use server::*;
pub use session::*;

use engram_core::sync::{MutexExt, RwLockExt};
use std::sync::{Arc, Mutex};

use engram_core::{
    Durability, EdgePatch, EdgeStatus, EdgeType, Engine, Error, Hub, NewEdge, NewNode, Node,
    NodePatch, NodeStatus, NodeType, Source, SuspectVerdict, WriteOutcome, registry,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, InitializeRequestParams, InitializeResult,
    ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents, ResourceTemplate,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

// Rides into every client session, so it carries only what a first write
// needs: the type roster and call shapes (issue #9). Mechanism lives in the
// tool descriptions and docs (Decision 00c0b7rdbcs4).
const INSTRUCTIONS: &str = "\
Engram is the project's durable reasoning/decision memory as an editable graph. \
First call `brief` for the canon digest; if its first line says the session is \
bound by fallback — or the briefed project isn't your workspace — call `brief` \
again with `project` set to your workspace's absolute path: it rebinds this \
session and returns THAT project's brief. A write refused as \"bound to the home \
graph by fallback\" means the same (a resumed or restarted session starts unbound): \
bind, then retry — every write verdict names the `project` it landed in. \
`search` before non-trivial work. \
Capture decisions silently as they happen — a feature request usually hides one. \
Node types (this graph's actual set: `describe_ontology`): Decision, Principle, \
Caution, Problem, Resolution, Insight, Intent, Anchor, Tombstone (a record \
that knowledge was deliberately removed — don't resurrect it). \
Edge verbs: about, because, answers, builds-on, replaces, conflicts-with, needs \
— \"from <verb> to\" must read as a sentence. \
Call shapes: add_note {\"type\": \"Decision\", \"title\": \"...\", \"body\": \
\"...\"}; then link {\"from\": \"<new-id>\", \"to\": \"<other-id>\", \"type\": \
\"because\"}. add_notes {\"notes\": [<add_note items>]} carries NO links — \
create first, link the returned ids in a second pass. \
If describe_ontology lists custom fields, pass {\"fields\": {\"name\": value}} \
on add_note/update_node (update merges; null deletes); refusals teach the \
roster — follow them. \
Every write's response is a verdict, not a receipt: {matched, created: false} \
= near-duplicate (merge via `update_node`; several notes with the same \
knowledge → `merge_nodes`); `warnings` = landed near contradicted/superseded/\
tombstoned canon (`tombstoned` = a person deliberately removed this; do not \
re-add or re-derive it unless the user says so — the warning's `note` says why); \
`suspects` = judge each with `resolve_suspect` (conflict | replaces | \
dismiss) NOW and tell the user about genuine contradictions — the one \
exception to silent capture. Judge the brief's suspected conflicts early too. \
To retire knowledge yourself (no successor, don't re-learn): add_note a \
Tombstone \"Removed: <title>\" with the why, then link {\"from\": <tombstone>, \
\"to\": <victim>, \"type\": \"replaces\"} — the victim is archived, traceably; \
hard delete stays user-only. \
How memory BEHAVES (ontology, custom fields, brief composition, history \
recording, policy knobs, models) is configured over the core's HTTP API, never \
MCP: `GET /guide` on the core (port in `.engram/daemon.json`) is the manual — \
read it when the user asks to change a setting, and once on a cold start to \
review the settings with them. \
Nodes carry computed `trust` and `stale` (verify before relying); only \
deliberate acts refresh trust — a still-true stale node wants `update_node`. \
Pinning is user-only; a `replaces` that would archive a pinned node is refused. \
Reuse the brief's recent tags before inventing new ones. \
`check_claim` checks a statement against canon (hints, never judgments); \
`timeline` walks a replaces chain; `audit` pages the mutation journal; \
`list_drift` finds broken code_refs. \
Tasks (`add_tasks` / `update_task` / `list_tasks`) are your working list \
BESIDE the graph — never searched, never knowledge: the brief's `## Tasks` \
section says what you were doing. One task per piece of work that spans \
sessions (a feature = a group with `parent: \"$1\"` children), never per \
edit; mark them done as you go; `progress` notes where you stopped. When one \
closes having settled something, capture it as a note and add the id to the \
task's refs. If a task tool answers that tasks are turned off, work without \
them in that graph. \
`project` on most tools: omit = this project; name/id/directory = that graph \
(cross-project capture into a sibling's own graph is encouraged); \"home\" = \
the user-level graph; search/check_claim take \"all\" (reads only — writes \
to `all` are refused). \
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
