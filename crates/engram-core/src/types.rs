use serde::{Deserialize, Serialize};

/// Declares a string-backed enum whose wire form (serde) and storage form
/// (`as_str`/`parse`) are the same canonical strings — the ones PLAN.md fixes.
macro_rules! str_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $s),+ }
            }
            pub fn parse(s: &str) -> crate::Result<Self> {
                match s {
                    $($s => Ok(Self::$variant),)+
                    _ => Err(crate::Error::Parse { kind: stringify!($name), value: s.to_string() }),
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Self::parse(&s).map_err(serde::de::Error::custom)
            }
        }
    };
}

str_enum!(NodeType {
    Principle => "Principle",
    Decision => "Decision",
    Caution => "Caution",
    Problem => "Problem",
    Resolution => "Resolution",
    Insight => "Insight",
    Intent => "Intent",
    Anchor => "Anchor",
});

str_enum!(EdgeType {
    About => "about",
    Because => "because",
    Answers => "answers",
    BuildsOn => "builds-on",
    Replaces => "replaces",
    ConflictsWith => "conflicts-with",
    Needs => "needs",
});

str_enum!(Durability {
    Stable => "stable",
    Episodic => "episodic",
    Volatile => "volatile",
});

str_enum!(Source {
    User => "user",
    Claude => "claude",
});

str_enum!(NodeStatus {
    Open => "open",
    Resolved => "resolved",
    Obsolete => "obsolete",
});

str_enum!(SuspectStatus {
    Suspected => "suspected",
    Confirmed => "confirmed",
    Dismissed => "dismissed",
});

str_enum!(SuspectVerdict {
    Conflict => "conflict",
    Replaces => "replaces",
    Dismiss => "dismiss",
});

str_enum!(EdgeStatus {
    Active => "active",
    Resolved => "resolved",
    Dismissed => "dismissed",
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub title: String,
    pub body: Option<String>,
    pub durability: Durability,
    pub source: Source,
    pub session_id: Option<String>,
    pub created_at: i64,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub status: Option<NodeStatus>,
    /// Last time retrieval surfaced this node (search hit / brief inclusion).
    /// Observability only — trust never reads it (exposure is not evidence).
    pub last_seen: Option<i64>,
    /// Last deliberate act that vouched for the node: an update or an explicit
    /// "Confirm still true". The unapproved trust anchor.
    #[serde(default)]
    pub confirmed_at: Option<i64>,
    /// Last explicit approval — user action, or assistant on user demand.
    pub approved_at: Option<i64>,
    /// When contradicting evidence landed (judged conflict, drifted refs) —
    /// starts the decay ramp on otherwise-flat stable knowledge. Cleared by
    /// any deliberate update or approval (repair = re-validation).
    #[serde(default)]
    pub demoted_at: Option<i64>,
    /// User-set constant trust (the pane's pin = 1.0). Overrides the computed
    /// value entirely; pinned nodes never decay, auto-archive, or demote.
    #[serde(default)]
    pub trust_override: Option<f64>,
    /// Computed at read time from the timestamps (policy::trust);
    /// never stored. Defaults exist only so old exports still import.
    #[serde(default)]
    pub trust: f64,
    #[serde(default)]
    pub stale: bool,
    pub code_refs: Vec<String>,
    /// Free-form slice labels (PLAN §10 tags): how the user cuts the graph
    /// (phases, concerns) — orthogonal to Anchors, which say what code a note
    /// is about. Normalized to kebab-case on write.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Node {
    /// The trust-relevant view of this node (see policy module docs).
    pub fn trust_inputs(&self) -> crate::policy::TrustInputs {
        crate::policy::TrustInputs {
            created_at: self.created_at,
            confirmed_at: self.confirmed_at,
            approved_at: self.approved_at,
            demoted_at: self.demoted_at,
            trust_override: self.trust_override,
            durability: self.durability,
            status: self.status,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewNode {
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    /// The knowledge's original date (unix seconds), for digesting or
    /// importing historical material — one clock, `created_at`, everywhere.
    /// Omitted = now. Future values are clamped to now at write time.
    #[serde(default)]
    pub created_at: Option<i64>,
    pub durability: Durability,
    pub source: Source,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub status: Option<NodeStatus>,
    #[serde(default)]
    pub code_refs: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NodePatch {
    /// Reclassification (PLAN §10 Phase 1): the type was Claude's guess,
    /// correcting it must not require delete-and-recreate.
    #[serde(default, rename = "type")]
    pub node_type: Option<NodeType>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub durability: Option<Durability>,
    #[serde(default)]
    pub status: Option<NodeStatus>,
    #[serde(default)]
    pub valid_until: Option<i64>,
    #[serde(default)]
    pub code_refs: Option<Vec<String>>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: String,
    #[serde(rename = "type")]
    pub edge_type: EdgeType,
    pub from_id: String,
    pub to_id: String,
    pub source: Source,
    pub created_at: i64,
    pub confidence: Option<f64>,
    pub strength: Option<f64>,
    pub note: Option<String>,
    pub valid_from: Option<i64>,
    pub valid_until: Option<i64>,
    pub status: Option<EdgeStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewEdge {
    #[serde(rename = "type")]
    pub edge_type: EdgeType,
    pub from_id: String,
    pub to_id: String,
    pub source: Source,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub strength: Option<f64>,
    #[serde(default)]
    pub status: Option<EdgeStatus>,
}

/// Portable, diffable snapshot of the whole graph (PLAN §6B JSON export).
/// Node/edge order is sorted by the exporter for stable git diffs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportGraph {
    pub version: u32,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

pub const EXPORT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct ImportSummary {
    pub nodes: usize,
    pub edges: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub title: String,
    pub snippet: String,
    pub score: f64,
    pub durability: Durability,
    pub status: Option<NodeStatus>,
    /// Computed trust at query time (policy::trust).
    pub trust: f64,
    /// Trust fell below the stale threshold — treat with suspicion and
    /// consider reconfirming or superseding.
    pub stale: bool,
    /// 1-hop subgraph around the match, `conflicts-with`/`replaces` first
    /// (PLAN §6A retrieval), capped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub neighbors: Vec<NeighborRef>,
    /// Which project the hit came from — set only on cross-project reads
    /// (PLAN §7C provenance); absent means the queried project itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// One edge+endpoint of a hit's 1-hop subgraph, compact enough to inline in
/// search results without blowing the token budget.
#[derive(Debug, Clone, Serialize)]
pub struct NeighborRef {
    pub edge_id: String,
    pub edge_type: EdgeType,
    /// "out": the hit points at this neighbor; "in": this neighbor points at the hit.
    pub direction: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_status: Option<EdgeStatus>,
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub title: String,
    /// The neighbor is superseded/archived (`valid_until` set).
    pub archived: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EdgePatch {
    /// Retype the edge (PLAN §10 pane CRUD): picking the wrong verb must not
    /// require delete-and-recreate.
    #[serde(default, rename = "type")]
    pub edge_type: Option<EdgeType>,
    #[serde(default)]
    pub status: Option<EdgeStatus>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub strength: Option<f64>,
}

/// One generation of a node's `replaces` chain (PLAN §10 timeline), oldest
/// first: how a piece of knowledge evolved into its current form.
#[derive(Debug, Clone, Serialize)]
pub struct TimelineEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub title: String,
    pub created_at: i64,
    /// Set when this generation was superseded (archived).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<i64>,
    /// The note on the `replaces` edge that superseded this generation —
    /// usually the why of the change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaced_note: Option<String>,
}

/// A node whose path-shaped `code_refs` no longer resolve against the project
/// root (PLAN §10 verified code refs): the code moved or was deleted and the
/// memory didn't follow — a contradiction between the graph and reality,
/// surfaced for review like a conflict.
#[derive(Debug, Clone, Serialize)]
pub struct Drift {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub title: String,
    /// The refs that no longer exist (a node's healthy refs are not listed).
    pub missing: Vec<String>,
}

/// A locally-detected candidate contradiction awaiting judgment (PLAN §7
/// conflict scan): two unlinked nodes close enough in embedding space to be
/// talking about the same thing. `a_id` is the newer node, `b_id` the older,
/// so a confirming `replaces` edge reads "a replaces b". Resolved rows stay
/// (confirmed/dismissed) so a judged pair is never re-raised.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suspect {
    pub id: String,
    pub a_id: String,
    pub b_id: String,
    pub similarity: f64,
    pub created_at: i64,
    pub status: SuspectStatus,
    /// Local-cortex triage hint: contradiction | entailment | neutral.
    /// A nomination for the judge, never a verdict (PLAN §7A).
    pub nli_label: Option<String>,
    pub nli_score: Option<f64>,
}

/// A pending suspect joined with what the judge (pane or Claude) needs to see.
#[derive(Debug, Clone, Serialize)]
pub struct SuspectView {
    pub id: String,
    pub similarity: f64,
    pub created_at: i64,
    /// Triage hint from the local NLI layer (suggests, never judges).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nli_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nli_score: Option<f64>,
    pub a: SuspectEndpoint,
    pub b: SuspectEndpoint,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuspectEndpoint {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub title: String,
}

/// One node's NLI verdict against a checked claim (PLAN §7A check_claim).
#[derive(Debug, Clone, Serialize)]
pub struct ClaimVerdict {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub title: String,
    pub trust: f64,
    pub stale: bool,
    pub entailment: f32,
    pub neutral: f32,
    pub contradiction: f32,
    /// Cross-project provenance (PLAN §7C) — absent for the queried project.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// The canon's answer to "is this claim true here": which nodes support it,
/// which contradict it, and which are merely nearby but silent. A gap (all
/// silent) is a capture opportunity, not an error.
#[derive(Debug, Clone, Serialize)]
pub struct ClaimReport {
    pub claim: String,
    pub supports: Vec<ClaimVerdict>,
    pub contradicts: Vec<ClaimVerdict>,
    pub silent: Vec<ClaimVerdict>,
}

/// What an audit sweep did: pairs it examined with NLI, suspects it queued,
/// and whether the pair budget cut it short (PLAN §7A: no silent caps).
#[derive(Debug, Clone, Serialize)]
pub struct AuditSweep {
    pub queued: usize,
    pub examined: usize,
    pub truncated: bool,
}

/// A nomination that an open Problem/Intent may already be answered by an
/// existing node — the judge links `answers` and resolves, or ignores.
#[derive(Debug, Clone, Serialize)]
pub struct AnsweredHint {
    pub problem: SuspectEndpoint,
    pub candidate: SuspectEndpoint,
    pub entailment: f64,
}

/// What a checked node update did — the same-turn verdict set every write
/// returns (PLAN §6A/§7A): near-canon warnings, freshly queued look-alike
/// suspects, and code_refs that don't resolve in the repo.
#[derive(Debug, Clone)]
pub struct CheckedUpdate {
    pub node: Node,
    pub warnings: Vec<WriteWarning>,
    pub suspects: Vec<SuspectView>,
    pub missing_refs: Vec<String>,
}

/// One row of the hub's project listing (PLAN §7C): the registry entries plus
/// the current project and the home graph, with liveness flags.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    pub db: String,
    /// The project this daemon/session was launched in.
    pub current: bool,
    /// The reserved user-level home graph.
    pub home: bool,
    /// An engine for this project is open in this process.
    pub open: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<i64>,
}

/// A promotion nomination (PLAN §7C): a Principle/Caution that recurs across
/// project graphs and may belong in the home graph. Nomination only — the
/// user approves; nothing self-applies.
#[derive(Debug, Clone, Serialize)]
pub struct PromotionCandidate {
    /// The current project's copy, in full (the pane promotes from this).
    pub node: Node,
    /// Same-type look-alikes of it in other projects.
    pub matches: Vec<PromotionMatch>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PromotionMatch {
    pub project: String,
    pub id: String,
    pub title: String,
    pub similarity: f64,
}

/// One tag with its usage stats — the pane's dropdown and the brief's
/// "recent tags" line both read from this (freshest first).
#[derive(Debug, Clone, Serialize)]
pub struct TagStat {
    pub tag: String,
    pub count: i64,
    pub last_used: i64,
}

/// Attached to a write result when the new text lands near contradicted or
/// superseded knowledge — the pull-based version of PLAN §7's conflict push.
#[derive(Debug, Clone, Serialize)]
pub struct WriteWarning {
    pub id: String,
    pub title: String,
    /// "in-active-conflict" | "superseded"
    pub reason: String,
    pub similarity: f64,
}

/// One row of the append-only audit journal (PLAN §10): a node/edge mutation
/// with full before/after snapshots plus the binary-side context of the
/// writing process. `seq` is the keyset-pagination cursor (newest = highest).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub seq: i64,
    pub ts: i64,
    /// created | updated | approved | unapproved | pinned | unpinned |
    /// demoted | undemoted | archived | deleted | imported
    pub action: String,
    /// node | edge | graph
    pub entity: String,
    pub entity_id: String,
    /// Display label snapshot — survives the entity's later deletion.
    pub title: Option<String>,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
    /// pane | mcp | daemon | cli | library
    pub origin: String,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub pid: Option<i64>,
    pub version: Option<String>,
}

/// One page of the journal, newest first, with the unfiltered-total so the
/// pane can show progress ("50 of 312").
#[derive(Debug, Clone, Serialize)]
pub struct AuditPage {
    pub entries: Vec<AuditEntry>,
    pub total: i64,
}

/// What a checked (Claude-side) note write did: created a node, or
/// short-circuited to an existing near-duplicate (PLAN Appendix A `add_note`).
#[derive(Debug, Clone)]
pub enum WriteOutcome {
    Created {
        node: Node,
        warnings: Vec<WriteWarning>,
        /// Look-alike pairs this write just queued — returned so the writer
        /// judges them in the same turn instead of leaving them for the next
        /// session's brief (PLAN §7: detection is local, judgment is the
        /// assistant's).
        suspects: Vec<SuspectView>,
        /// Path-shaped code_refs that don't resolve right now — the
        /// write-time half of the drift check, caught in the same turn.
        missing_refs: Vec<String>,
    },
    Matched {
        node: Node,
        similarity: f64,
        /// NLI verdict on (new text, existing node) — at duplicate-level
        /// similarity co-reference holds, so a `contradiction` here means a
        /// NEGATED near-duplicate ("use X" vs "don't use X"): the one case
        /// where merging blindly would corrupt the canon.
        nli_label: Option<String>,
        nli_score: Option<f64>,
    },
}

/// Backend-neutral store facts for `/system` and doctor — what used to require
/// reaching through `conn()` into raw SQLite.
#[derive(Debug, Clone, Serialize)]
pub struct StoreStats {
    /// Which driver backs this store: `"sqlite"` or `"tepindb"`.
    pub backend: &'static str,
    pub nodes: i64,
    pub edges: i64,
    /// Nodes with a stored embedding vector.
    pub embedded: i64,
}

/// Backend-reported integrity, replacing doctor's raw PRAGMA probes.
#[derive(Debug, Clone, Serialize)]
pub struct StoreHealth {
    /// SQLite's journal mode (`wal` expected); `None` for backends without one.
    pub journal_mode: Option<String>,
    pub integrity_ok: bool,
    /// Human-readable detail when integrity is not ok.
    pub detail: Option<String>,
}

/// The embedding model identity a store's vectors were computed with — the
/// guard that triggers a full re-embed when the active model changes
/// (PLAN §7A model selection; TepinDB pins the same pair per file).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbedModelId {
    pub name: String,
    pub dim: usize,
}
