//! Per-graph configuration: ontology, policy numbers and brief composition as
//! one document stored in the graph (it travels with `migrate` and exports).
//! Edited in the pane and over `GET/PUT /projects/{sel}/config`; no MCP write
//! surface, since reshaping the ontology is a user gesture.
//!
//! **Roles, never names:** engine logic keys on role flags (`worklist`,
//! `supersession`, `contradiction`, …), so a renamed ontology keeps every
//! behaviour. Any configuration keeps sentence-shaped edges and exactly one
//! supersession and one contradiction verb. `GraphConfig::default()` is the
//! shipped 9-type/7-verb ontology and constants.

use serde::{Deserialize, Serialize};

use crate::types::Durability;

mod graph;
mod policy_config;
mod presets;
mod validate;

pub use policy_config::*;
pub use presets::*;

// ---------------------------------------------------------------------------
// document
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GraphConfig {
    #[serde(default)]
    pub ontology: OntologyConfig,
    #[serde(default)]
    pub policy: PolicyConfig,
    #[serde(default)]
    pub brief: BriefConfig,
    #[serde(default)]
    pub versioning: VersioningConfig,
    #[serde(default)]
    pub history: HistoryConfig,
    /// User-defined custom fields on notes: first-class values beside
    /// `created_at`/`tags`, per-graph, enforced at the engine write boundary.
    /// Empty (the default) = the feature is invisible.
    #[serde(default)]
    pub fields: Vec<FieldDef>,
    /// The working list beside the graph — on by default; a graph
    /// that doesn't want it turns it off here.
    #[serde(default)]
    pub tasks: TasksConfig,
}

/// Tasks. Off = no brief section, no rule settling, and the task
/// tools refuse with a message that tells the assistant what to do instead;
/// stored tasks are kept, so turning it back on restores the list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TasksConfig {
    pub enabled: bool,
}

impl Default for TasksConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// One user-defined field on notes. Definitions live in the graph's
/// config (edited in the pane / `PUT /config` — like the ontology, never over
/// MCP); *values* live on each node's `fields` map and are validated at the
/// engine write boundary, so every surface gets the same contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FieldDef {
    /// Storage + wire key: lowercase snake_case, 1..=32 chars, unique, never
    /// one of the built-in node field names (validation enforces all of it).
    pub name: String,
    /// Pane display label; empty = render the name.
    #[serde(default)]
    pub label: String,
    /// Value kind: text | number | bool | date | enum | url.
    pub kind: FieldKind,
    /// The allowed values when `kind` is `enum` (must be non-empty then).
    #[serde(default)]
    pub values: Vec<String>,
    /// Refuse writes of applicable types that omit this field.
    #[serde(default)]
    pub required: bool,
    /// Type names this field applies to; empty = every type.
    #[serde(default)]
    pub applies_to: Vec<String>,
    /// Join the search index — the value is embedded with the note and enters
    /// the keyword channel. Tepin-backed graphs only (the sqlite driver is a
    /// legacy migration source and ignores it).
    #[serde(default)]
    pub indexed: bool,
    /// Render `name: value` on this note's brief lines.
    #[serde(default)]
    pub show_in_brief: bool,
}

/// What a custom field's value must be. `date` accepts a `"YYYY-MM-DD"` day,
/// an ISO instant string, or unix seconds; `url` a `scheme://` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Text,
    Number,
    Bool,
    Date,
    Enum,
    Url,
}

impl FieldKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FieldKind::Text => "text",
            FieldKind::Number => "number",
            FieldKind::Bool => "bool",
            FieldKind::Date => "date",
            FieldKind::Enum => "enum",
            FieldKind::Url => "url",
        }
    }
}

/// Built-in node field names a custom field may never shadow: every serde key
/// of [`crate::types::Node`] plus the computed/response-level keys the API
/// attaches (`born_in`, `archived`, `pinned`, and the `fields` container
/// itself). Collision would make a node's JSON ambiguous.
pub const RESERVED_FIELD_NAMES: &[&str] = &[
    "id",
    "type",
    "title",
    "body",
    "durability",
    "source",
    "session_id",
    "created_at",
    "valid_from",
    "valid_until",
    "status",
    "last_seen",
    "confirmed_at",
    "approved_at",
    "demoted_at",
    "trust_override",
    "trust",
    "stale",
    "code_refs",
    "tags",
    "version",
    "props",
    "fields",
    "born_in",
    "archived",
    "pinned",
    "score",
    "snippet",
    "neighbors",
];

/// The history layer: chat transcripts harvested into a sibling
/// `history.tepin`. Opt-in (sealing keys live in the OS keystore), and every
/// knob is a pane gesture (Settings → History).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryConfig {
    /// Master switch: harvest this project's sessions at all. Off by
    /// default — enabling is the user's gesture (pane).
    #[serde(default)]
    pub enabled: bool,
    /// Per-harness ingestion switches — a harness the user turns off is
    /// skipped by the harvester even when its transcripts are discovered.
    #[serde(default)]
    pub harnesses: HarnessToggles,
    /// Absolute paths (files or directories) the harvester must never read,
    /// on top of the harness toggles.
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    /// May history answer as the sectioned search fall-through. Off = history
    /// is ingested and browsable but never surfaces in `search`.
    #[serde(default = "default_true")]
    pub search_fallthrough: bool,
    /// Cosine at which `order: "recent"` folds an older history hit under a newer
    /// one. Nothing is dropped, so a mistuned value costs shape, not recall; a
    /// starting value, not a measured one, and per-graph because thresholds don't
    /// transfer between graphs (00b22705tdob).
    #[serde(default = "default_recency_collapse")]
    pub recency_collapse: f64,
}

fn default_recency_collapse() -> f64 {
    0.9
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            harnesses: HarnessToggles::default(),
            exclude_paths: Vec::new(),
            search_fallthrough: true,
            recency_collapse: default_recency_collapse(),
        }
    }
}

/// One switch per harness, aligned with the main agent roster
/// ([`crate::harness::AGENTS`]): claude, codex, gemini-cli, opencode, kilo,
/// antigravity. All on by default once recording is enabled; a later-wave
/// harness gets a field here when its adapter lands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HarnessToggles {
    #[serde(default = "default_true")]
    pub claude_code: bool,
    #[serde(default = "default_true")]
    pub codex: bool,
    #[serde(default = "default_true")]
    pub gemini: bool,
    #[serde(default = "default_true")]
    pub opencode: bool,
    #[serde(default = "default_true")]
    pub kilo: bool,
    #[serde(default = "default_true")]
    pub antigravity: bool,
    /// IBM Bob — one switch for both surfaces (IDE + BobShell), which share
    /// one on-disk store.
    #[serde(default = "default_true")]
    pub bob: bool,
}

impl Default for HarnessToggles {
    fn default() -> Self {
        Self {
            claude_code: true,
            codex: true,
            gemini: true,
            opencode: true,
            kilo: true,
            antigravity: true,
            bob: true,
        }
    }
}

/// Version tracking: the graph carries a current working version (set via
/// `set_version` or the pane) and stamps it on new nodes of version-bound
/// types. Display and provenance only; trust and ranking ignore it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct VersioningConfig {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OntologyConfig {
    /// Which preset this ontology started from ("engram", …, or "custom") —
    /// provenance for the pane and exports, never read by engine logic.
    pub preset: String,
    pub types: Vec<TypeDef>,
    pub verbs: Vec<VerbDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TypeDef {
    /// Display + storage name ("Decision"). Renaming is a skinning operation;
    /// stored nodes are bulk-retyped by the ontology editor, never here.
    pub name: String,
    /// The one color input (0..360) — light/dark schemes, neutrals and text
    /// colors are all derived from it, per project.
    pub hue: u16,
    /// The thought this type captures — the teaching line `describe_ontology`
    /// and the brief's ontology section render.
    pub thought: String,
    /// Default durability for new nodes of this type.
    pub durability: Durability,
    pub roles: TypeRoles,
    /// This type's canon section in the brief (worklist types surface through
    /// the open-work section instead — see `BriefConfig`).
    pub brief: BriefSection,
}

/// What the engine may know about a type. Every flag is a behavior contract,
/// not a label — adding one here means some engine path reads it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TypeRoles {
    /// Open/resolved lifecycle: lives in the open-work worklist and never
    /// decays while open (Problem/Intent in the shipped set).
    #[serde(default)]
    pub worklist: bool,
    /// A code-subject node: carries `code_refs` as its identity and anchors
    /// `about` edges (Anchor in the shipped set).
    #[serde(default)]
    pub anchor: bool,
    /// Ranking prior added inside trust_boost — type weighting lives in
    /// ranking, never in trust itself (trust v2 rule). 0 = none.
    #[serde(default)]
    pub rank_prior: f64,
    /// May the pane accent/highlight nodes of this type.
    #[serde(default = "default_true")]
    pub highlight: bool,
    /// Whether nodes of this type bind to the current working version when
    /// version tracking is on. Off for types that transcend releases
    /// (Principle and Anchor in the shipped set — values and code subjects
    /// aren't release artifacts).
    #[serde(default = "default_true")]
    pub versioned: bool,
    /// A deletion marker, so removed knowledge isn't re-learned. Findable, but
    /// out of the conflict scan and answer candidacy; nearby writes get a
    /// `tombstoned` warning and `check_claim` files it as `retracted`. Born when
    /// an authored tombstone `replaces` a live victim, or when the pane's hard
    /// delete mints one.
    #[serde(default)]
    pub tombstone: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerbDef {
    /// The sentence verb ("because", "conflicts-with") — a triple must read
    /// as plain English, so verbs are lowercase, hyphen-joined words.
    pub name: String,
    /// A worked example, for teaching surfaces ("Decision because Principle").
    pub reads_as: String,
    pub roles: VerbRoles,
}

/// Behavior contracts per verb. Exactly one verb carries `supersession` and
/// exactly one carries `contradiction` — the two edges that make the graph
/// active are roles the redactor can move, never remove.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct VerbRoles {
    /// Creating it archives the older endpoint and chains history
    /// (`replaces` in the shipped set).
    #[serde(default)]
    pub supersession: bool,
    /// A judged one demotes the older endpoint's trust and feeds the
    /// conflict worklist (`conflicts-with` in the shipped set).
    #[serde(default)]
    pub contradiction: bool,
    /// The reason-edge (`because`): its absence on a reasoning node is what
    /// the structural checkup flags.
    #[serde(default)]
    pub reason: bool,
    /// Closes worklist nodes (`answers`): Resolution answers Problem.
    #[serde(default)]
    pub answer: bool,
    /// A live dependency that keeps worklist edges active (`needs`).
    #[serde(default)]
    pub dependency: bool,
    /// The source holds what the target holds: a contradiction or supersession of
    /// the target queues the source for review. Unset = on for the `dependency`
    /// and `reason` verbs and `builds-on`. See [`VerbDef::inherits`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherits: Option<bool>,
}

impl VerbDef {
    /// Whether an edge of this verb carries a contradiction of its target
    /// back to its source (the explicit role, else the derived default).
    pub fn inherits(&self) -> bool {
        self.roles
            .inherits
            .unwrap_or(self.roles.dependency || self.roles.reason || self.name == "builds-on")
    }
}

// ---------------------------------------------------------------------------
// policy — the dogfood-tunable numbers (mirrors policy.rs defaults)
// ---------------------------------------------------------------------------

/// The reserved tag that marks a note as a session handoff: "the NEXT
/// session must read this first". A cross-surface protocol token (the skills
/// and generated skills teach the same literal), so it is a constant, not a
/// config knob.
pub const HANDOFF_TAG: &str = "handoff";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BriefConfig {
    /// Character budget for the whole digest (~4 chars/token).
    pub total_chars: usize,
    /// The "Recent tags" vocabulary line.
    pub tags: BriefToggle,
    /// The unresolved-conflicts section (uncapped: judged evidence is the
    /// single highest-value content the brief carries).
    pub conflicts: BriefToggle,
    /// Suspected conflicts awaiting judgment.
    pub suspects: BriefToggle,
    /// The "Recently added" window.
    pub recent: BriefSection,
    /// The open worklist (every type with the `worklist` role).
    pub open: BriefSection,
    /// Budget reserved for the home-graph section riding along.
    pub home_reserve: usize,
    /// Teach the graph's ontology at the top of the brief — for graphs whose
    /// ontology the assistant can't know from its skill (off in the shipped
    /// preset; `describe_ontology` serves the same content on demand).
    pub ontology: BriefToggle,
    /// The [`HANDOFF_TAG`] section: open worklist notes left for the next
    /// session — guaranteed first placement, generous excerpts.
    #[serde(default = "default_handoff")]
    pub handoff: BriefSection,
    /// Current cycle: open worklist notes stamped with the working version, right
    /// under the version line. Renders nothing without a working version.
    #[serde(default = "default_cycle")]
    pub cycle: BriefSection,
    /// Whether entries carry a body excerpt after the title (default on). Off
    /// buys more entries when titles are already whole claims.
    #[serde(default = "default_true")]
    pub bodies: bool,
    /// How each type's canon section picks and orders its entries:
    /// `endorsed` (default) = pinned, then approved, then newest endorsement
    /// or capture; `connected` = pinned first, then trust weighted by how
    /// connected the note is (live edges), so the load-bearing canon leads.
    #[serde(default = "default_canon_order")]
    pub canon_order: String,
    /// The `## Tasks` section: the active working list, titles only,
    /// right after the current cycle.
    #[serde(default)]
    pub tasks: BriefTasks,
}

/// The brief's task section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BriefTasks {
    pub show: bool,
    /// Most active top-level tasks (groups count once) listed.
    pub cap: usize,
    /// How many recently done titles ride along (0 = none).
    pub recent_done: usize,
    /// Sessions that briefed without an open task changing before it reads
    /// as idle (0 = never).
    pub idle_sessions: usize,
}

impl Default for BriefTasks {
    fn default() -> Self {
        Self {
            show: true,
            cap: 15,
            recent_done: 3,
            idle_sessions: 5,
        }
    }
}

fn default_canon_order() -> String {
    "endorsed".to_string()
}

fn default_handoff() -> BriefSection {
    BriefSection {
        show: true,
        cap: 10,
        excerpt: 400,
    }
}

fn default_cycle() -> BriefSection {
    BriefSection {
        show: true,
        cap: 8,
        excerpt: 400,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BriefToggle {
    pub show: bool,
    /// Max entries (ignored where a section is uncapped).
    pub cap: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BriefSection {
    pub show: bool,
    pub cap: usize,
    /// Per-entry body excerpt length, in characters.
    pub excerpt: usize,
}

impl Default for BriefConfig {
    fn default() -> Self {
        Self {
            total_chars: crate::policy::DEFAULT_BRIEF_CHARS,
            tags: BriefToggle { show: true, cap: 7 },
            conflicts: BriefToggle { show: true, cap: 0 },
            suspects: BriefToggle { show: true, cap: 8 },
            recent: BriefSection {
                show: true,
                cap: 7,
                excerpt: 140,
            },
            open: BriefSection {
                show: true,
                cap: 10,
                excerpt: 140,
            },
            home_reserve: crate::policy::HOME_BRIEF_RESERVE,
            ontology: BriefToggle {
                show: false,
                cap: 0,
            },
            handoff: default_handoff(),
            cycle: default_cycle(),
            bodies: true,
            canon_order: default_canon_order(),
            tasks: BriefTasks::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// the shipped preset — today's ontology, verbatim
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

impl TypeRoles {
    pub(crate) fn plain(rank_prior: f64) -> Self {
        Self {
            worklist: false,
            anchor: false,
            rank_prior,
            highlight: true,
            versioned: true,
            tombstone: false,
        }
    }
}
