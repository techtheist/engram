//! Per-graph configuration (PLAN §7D): the ontology (node types + edge
//! verbs), the policy numbers (trust/decay/thresholds), and the brief
//! composition as ONE document stored in the graph itself — it travels with
//! `migrate` and rides along in exports. Edited in the pane's Settings menu
//! and over `GET/PUT /projects/{sel}/config`; deliberately NO MCP write
//! surface — reshaping the ontology is a user gesture, like pin and delete.
//!
//! **Roles, never names** (the release's governing rule): engine logic keys
//! on the role flags a type or verb carries — `worklist`, `supersession`,
//! `contradiction` — never on the strings, so a renamed or swapped ontology
//! keeps every behavior. Hard invariants hold across ANY configuration:
//! edges stay sentence-shaped, and exactly one supersession verb plus one
//! contradiction verb must exist (they are what make the graph active).
//!
//! `GraphConfig::default()` reproduces the shipped 8-type/7-verb ontology and
//! today's constants exactly — a graph with no stored config behaves
//! identically to 0.6.x.

use serde::{Deserialize, Serialize};

use crate::types::Durability;

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
}

/// Version tracking (0.7.0): when enabled, the graph carries a CURRENT
/// WORKING VERSION (store meta, set via the `set_version` MCP tool or the
/// pane) and every new node of a version-bound type is stamped with it —
/// "this was captured at v0.5.1". The stamp is display + provenance only;
/// nothing in trust or ranking reads it.
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
    /// colors are all derived from it, per project (PLAN §7D stage 4).
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
}

// ---------------------------------------------------------------------------
// policy — the dogfood-tunable numbers (mirrors policy.rs defaults)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PolicyConfig {
    /// Trust anchors: created → confirmed → approved (0..=1, ordered).
    pub trust_created: f64,
    pub trust_confirmed: f64,
    pub trust_approved: f64,
    /// Where approved trust bottoms out — approval never fully expires.
    pub trust_approved_floor: f64,
    /// Where unapproved trust bottoms out.
    pub trust_floor: f64,
    /// Below this computed trust a node is stale (verify before relying).
    pub stale_trust: f64,
    /// Unapproved episodic trust runs start→floor over this many days.
    pub episodic_window_days: i64,
    /// Volatile notes rot fast: days from start to floor.
    pub volatile_window_days: i64,
    /// Approved (non-stable) trust runs its course over this many days.
    pub approved_window_days: i64,
    /// Days below stale before the decay pass archives a provisional node.
    pub decay_ttl_days: i64,
    /// Same-type cosine at/above which add_note returns the match instead.
    pub duplicate_similarity: f64,
    /// Cosine at/above which two unlinked nodes become a suspected conflict.
    pub conflict_suspect_similarity: f64,
    /// Similarity at/above which a write warns about conflicted/superseded
    /// neighbors.
    pub warn_similarity: f64,
    /// Minimum NLI confidence before an audit sweep queues a pair.
    pub nli_sweep_min_confidence: f64,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        use crate::policy::*;
        Self {
            trust_created: TRUST_UNSEEN_START,
            trust_confirmed: TRUST_CONFIRMED_START,
            trust_approved: TRUST_APPROVED_START,
            trust_approved_floor: TRUST_APPROVED_FLOOR,
            trust_floor: TRUST_FLOOR,
            stale_trust: STALE_TRUST,
            episodic_window_days: PROVISIONAL_TRUST_WINDOW_SECS / 86_400,
            volatile_window_days: VOLATILE_TRUST_WINDOW_SECS / 86_400,
            approved_window_days: APPROVED_TRUST_WINDOW_SECS / 86_400,
            decay_ttl_days: DECAY_TTL_DAYS,
            duplicate_similarity: DUPLICATE_SIMILARITY,
            conflict_suspect_similarity: CONFLICT_SUSPECT_SIMILARITY,
            warn_similarity: WARN_SIMILARITY,
            nli_sweep_min_confidence: NLI_SWEEP_MIN_CONFIDENCE as f64,
        }
    }
}

// ---------------------------------------------------------------------------
// brief — full composition control (mirrors the engine's section constants)
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
}

fn default_handoff() -> BriefSection {
    BriefSection {
        show: true,
        cap: 10,
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
        }
    }
}

// ---------------------------------------------------------------------------
// the shipped preset — today's ontology, verbatim
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

fn tdef(
    name: &str,
    hue: u16,
    thought: &str,
    durability: Durability,
    roles: TypeRoles,
    brief: BriefSection,
) -> TypeDef {
    TypeDef {
        name: name.into(),
        hue,
        thought: thought.into(),
        durability,
        roles,
        brief,
    }
}

fn vdef(name: &str, reads_as: &str, roles: VerbRoles) -> VerbDef {
    VerbDef {
        name: name.into(),
        reads_as: reads_as.into(),
        roles,
    }
}

fn hidden_brief() -> BriefSection {
    BriefSection {
        show: false,
        cap: 8,
        excerpt: 140,
    }
}

fn shown_brief(cap: usize, excerpt: usize) -> BriefSection {
    BriefSection {
        show: true,
        cap,
        excerpt,
    }
}

/// One shipped ontology template (PLAN §7D stage 4): a complete, valid
/// config a user can start from. Applying one is just `PUT /config` with
/// its document — the pane's "start from preset" gesture.
#[derive(Debug, Clone, Serialize)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub description: String,
    pub config: GraphConfig,
}

/// The curated preset shelf. `engram` is the default 8-type product-building
/// set every graph is born with; the others reshape the ontology for a
/// different kind of work. All satisfy the hard invariants by construction
/// (asserted by test).
pub fn presets() -> Vec<Preset> {
    let research = GraphConfig {
        ontology: OntologyConfig {
            preset: "research".into(),
            types: vec![
                tdef(
                    "Claim",
                    217,
                    "something believed, with evidence",
                    Durability::Stable,
                    TypeRoles::plain(0.05),
                    shown_brief(8, 140),
                ),
                tdef(
                    "Method",
                    292,
                    "how the investigation works",
                    Durability::Stable,
                    TypeRoles::plain(0.04),
                    shown_brief(5, 140),
                ),
                tdef(
                    "Question",
                    0,
                    "an open question to investigate",
                    Durability::Episodic,
                    TypeRoles {
                        worklist: true,
                        ..TypeRoles::plain(0.0)
                    },
                    hidden_brief(),
                ),
                tdef(
                    "Finding",
                    142,
                    "what an investigation produced",
                    Durability::Episodic,
                    TypeRoles::plain(0.04),
                    hidden_brief(),
                ),
                tdef(
                    "Source",
                    215,
                    "a paper, dataset or reference",
                    Durability::Stable,
                    TypeRoles {
                        anchor: true,
                        versioned: false,
                        ..TypeRoles::plain(0.0)
                    },
                    hidden_brief(),
                ),
                tdef(
                    "Task",
                    199,
                    "do this later",
                    Durability::Volatile,
                    TypeRoles {
                        worklist: true,
                        ..TypeRoles::plain(0.0)
                    },
                    hidden_brief(),
                ),
            ],
            verbs: vec![
                vdef("cites", "Claim cites Source", VerbRoles::default()),
                vdef(
                    "because",
                    "Claim because Finding",
                    VerbRoles {
                        reason: true,
                        ..VerbRoles::default()
                    },
                ),
                vdef(
                    "answers",
                    "Finding answers Question",
                    VerbRoles {
                        answer: true,
                        ..VerbRoles::default()
                    },
                ),
                vdef(
                    "builds-on",
                    "Finding builds-on Finding",
                    VerbRoles::default(),
                ),
                vdef(
                    "supersedes",
                    "Claim supersedes Claim",
                    VerbRoles {
                        supersession: true,
                        ..VerbRoles::default()
                    },
                ),
                vdef(
                    "refutes",
                    "Finding refutes Claim",
                    VerbRoles {
                        contradiction: true,
                        ..VerbRoles::default()
                    },
                ),
                vdef(
                    "needs",
                    "Task needs Finding",
                    VerbRoles {
                        dependency: true,
                        ..VerbRoles::default()
                    },
                ),
            ],
        },
        ..GraphConfig::default()
    };

    let minimal = GraphConfig {
        ontology: OntologyConfig {
            preset: "minimal".into(),
            types: vec![
                tdef(
                    "Rule",
                    38,
                    "a standing rule or preference",
                    Durability::Stable,
                    TypeRoles {
                        versioned: false,
                        ..TypeRoles::plain(0.05)
                    },
                    shown_brief(8, 140),
                ),
                tdef(
                    "Note",
                    217,
                    "anything worth keeping",
                    Durability::Episodic,
                    TypeRoles::plain(0.0),
                    shown_brief(10, 140),
                ),
                tdef(
                    "Todo",
                    199,
                    "do this later",
                    Durability::Volatile,
                    TypeRoles {
                        worklist: true,
                        ..TypeRoles::plain(0.0)
                    },
                    hidden_brief(),
                ),
            ],
            verbs: vec![
                vdef(
                    "because",
                    "Rule because Note",
                    VerbRoles {
                        reason: true,
                        ..VerbRoles::default()
                    },
                ),
                vdef(
                    "replaces",
                    "Note replaces Note",
                    VerbRoles {
                        supersession: true,
                        ..VerbRoles::default()
                    },
                ),
                vdef(
                    "conflicts-with",
                    "Note conflicts-with Rule",
                    VerbRoles {
                        contradiction: true,
                        ..VerbRoles::default()
                    },
                ),
            ],
        },
        ..GraphConfig::default()
    };

    vec![
        Preset {
            id: "engram".into(),
            name: "Engram".into(),
            description: "The shipped product-building set: decisions, principles, cautions, \
                          problems and their resolutions, insights, intents, code anchors."
                .into(),
            config: GraphConfig::default(),
        },
        Preset {
            id: "research".into(),
            name: "Research".into(),
            description: "For investigation-shaped work: claims with evidence, open questions, \
                          findings, sources, methods — refutes carries the contradiction role."
                .into(),
            config: research,
        },
        Preset {
            id: "minimal".into(),
            name: "Minimal".into(),
            description: "Three types — rules, notes, todos — for graphs that want almost no \
                          ceremony."
                .into(),
            config: minimal,
        },
    ]
}

impl TypeRoles {
    fn plain(rank_prior: f64) -> Self {
        Self {
            worklist: false,
            anchor: false,
            rank_prior,
            highlight: true,
            versioned: true,
        }
    }
}

impl Default for OntologyConfig {
    fn default() -> Self {
        // The same builders the preset shelf uses — one vocabulary for
        // declaring ontologies.
        let (t, v) = (tdef, vdef);
        let hidden = hidden_brief();
        let shown = shown_brief;
        let none = VerbRoles::default();
        Self {
            preset: "engram".into(),
            types: vec![
                t(
                    "Principle",
                    258,
                    "this is how I like things / what I value",
                    Durability::Stable,
                    TypeRoles {
                        versioned: false,
                        ..TypeRoles::plain(0.05)
                    },
                    shown(8, 140),
                ),
                t(
                    "Decision",
                    217,
                    "we chose this, for a reason",
                    Durability::Stable,
                    TypeRoles::plain(0.04),
                    shown(7, 80),
                ),
                t(
                    "Caution",
                    38,
                    "watch out — this bites",
                    Durability::Stable,
                    TypeRoles::plain(0.05),
                    shown(10, 140),
                ),
                t(
                    "Problem",
                    0,
                    "this was hard / went wrong",
                    Durability::Episodic,
                    TypeRoles {
                        worklist: true,
                        ..TypeRoles::plain(0.0)
                    },
                    hidden.clone(),
                ),
                t(
                    "Resolution",
                    142,
                    "here's how it got solved",
                    Durability::Episodic,
                    TypeRoles::plain(0.0),
                    hidden.clone(),
                ),
                t(
                    "Insight",
                    292,
                    "I realized something non-obvious",
                    Durability::Episodic,
                    TypeRoles::plain(0.04),
                    hidden.clone(),
                ),
                t(
                    "Intent",
                    199,
                    "do this later",
                    Durability::Volatile,
                    TypeRoles {
                        worklist: true,
                        ..TypeRoles::plain(0.0)
                    },
                    hidden.clone(),
                ),
                t(
                    "Anchor",
                    215,
                    "what this is about — a code subject",
                    Durability::Stable,
                    TypeRoles {
                        anchor: true,
                        versioned: false,
                        ..TypeRoles::plain(0.0)
                    },
                    hidden,
                ),
            ],
            verbs: vec![
                v("about", "Insight about Anchor", none.clone()),
                v(
                    "because",
                    "Decision because Principle",
                    VerbRoles {
                        reason: true,
                        ..none.clone()
                    },
                ),
                v(
                    "answers",
                    "Resolution answers Problem",
                    VerbRoles {
                        answer: true,
                        ..none.clone()
                    },
                ),
                v("builds-on", "Insight builds-on Insight", none.clone()),
                v(
                    "replaces",
                    "Decision replaces Decision",
                    VerbRoles {
                        supersession: true,
                        ..none.clone()
                    },
                ),
                v(
                    "conflicts-with",
                    "Insight conflicts-with Decision",
                    VerbRoles {
                        contradiction: true,
                        ..none.clone()
                    },
                ),
                v(
                    "needs",
                    "Intent needs Decision",
                    VerbRoles {
                        dependency: true,
                        ..none
                    },
                ),
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// validation — the hard invariants (PLAN §7D)
// ---------------------------------------------------------------------------

impl GraphConfig {
    /// Parse a stored document into the live config. `None` means the graph
    /// never customized; a corrupt document reads as defaults with a warning —
    /// config can never brick an open.
    pub fn from_stored(raw: Option<&str>) -> Self {
        let Some(raw) = raw else {
            return Self::default();
        };
        match serde_json::from_str(raw) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("engram: couldn't read graph config, using defaults: {e}");
                Self::default()
            }
        }
    }

    /// The type definition for `name`, if the ontology declares it.
    pub fn type_def(&self, name: &str) -> Option<&TypeDef> {
        self.ontology.types.iter().find(|t| t.name == name)
    }

    /// The verb definition for `name`, if the ontology declares it.
    pub fn verb_def(&self, name: &str) -> Option<&VerbDef> {
        self.ontology.verbs.iter().find(|v| v.name == name)
    }

    /// Names of the types carrying the `worklist` role (open/resolved
    /// lifecycle; Problem/Intent in the shipped set).
    pub fn worklist_types(&self) -> Vec<&str> {
        self.ontology
            .types
            .iter()
            .filter(|t| t.roles.worklist)
            .map(|t| t.name.as_str())
            .collect()
    }

    /// The one verb carrying the `supersession` role (`replaces` in the
    /// shipped set). Validation guarantees exactly one exists.
    pub fn supersession_verb(&self) -> &str {
        self.ontology
            .verbs
            .iter()
            .find(|v| v.roles.supersession)
            .map(|v| v.name.as_str())
            .unwrap_or("replaces")
    }

    /// The one verb carrying the `contradiction` role (`conflicts-with` in
    /// the shipped set). Validation guarantees exactly one exists.
    pub fn contradiction_verb(&self) -> &str {
        self.ontology
            .verbs
            .iter()
            .find(|v| v.roles.contradiction)
            .map(|v| v.name.as_str())
            .unwrap_or("conflicts-with")
    }

    /// The supersession verb as an [`EdgeType`], for creating edges.
    pub fn supersession_edge(&self) -> crate::types::EdgeType {
        crate::types::EdgeType::parse(self.supersession_verb())
            .unwrap_or(crate::types::EdgeType::Replaces)
    }

    /// The contradiction verb as an [`EdgeType`], for creating edges.
    pub fn contradiction_edge(&self) -> crate::types::EdgeType {
        crate::types::EdgeType::parse(self.contradiction_verb())
            .unwrap_or(crate::types::EdgeType::ConflictsWith)
    }

    /// A compact teaching rendition of this graph's ontology — the same
    /// content whether it rides at the top of the brief (the optional
    /// `brief.ontology` section) or answers the `describe_ontology` MCP tool.
    /// For graphs whose ontology the assistant can't know from its skill.
    pub fn describe_ontology(&self) -> String {
        fn paren(roles: &[&str]) -> String {
            if roles.is_empty() {
                String::new()
            } else {
                format!(" ({})", roles.join("; "))
            }
        }
        let mut out = String::from("This graph's ontology — node types:\n");
        for t in &self.ontology.types {
            let mut roles = Vec::new();
            if t.roles.worklist {
                roles.push("worklist: carries open/resolved status");
            }
            if t.roles.anchor {
                roles.push("anchor: a code subject, carries code_refs");
            }
            out.push_str(&format!(
                "- {} — \"{}\"; default durability {}{}\n",
                t.name,
                t.thought,
                t.durability.as_str(),
                paren(&roles)
            ));
        }
        out.push_str("Edge verbs (a triple must read as English):\n");
        for v in &self.ontology.verbs {
            let mut roles = Vec::new();
            if v.roles.supersession {
                roles.push("supersession: archives the older endpoint");
            }
            if v.roles.contradiction {
                roles.push("contradiction: flags a conflict, demotes trust");
            }
            if v.roles.reason {
                roles.push("the reason edge");
            }
            if v.roles.answer {
                roles.push("closes worklist nodes");
            }
            if v.roles.dependency {
                roles.push("a live dependency");
            }
            out.push_str(&format!(
                "- {} — e.g. {}{}\n",
                v.name,
                v.reads_as,
                paren(&roles)
            ));
        }
        out
    }

    /// Every invariant that must hold across ANY preset or edit. Returns the
    /// first violation as the error message a 400 carries.
    pub fn validate(&self) -> crate::Result<()> {
        let fail = |msg: String| Err(crate::Error::Config(msg));

        let types = &self.ontology.types;
        if types.is_empty() {
            return fail("ontology needs at least one node type".into());
        }
        let mut seen = std::collections::HashSet::new();
        for t in types {
            let name = t.name.trim();
            if name.is_empty() || name.len() > 32 {
                return fail(format!("type name {:?} must be 1..=32 chars", t.name));
            }
            if name != t.name {
                return fail(format!("type name {:?} has surrounding whitespace", t.name));
            }
            if !seen.insert(name.to_lowercase()) {
                return fail(format!("duplicate type name {name:?}"));
            }
            if t.hue >= 360 {
                return fail(format!("type {name}: hue {} out of 0..360", t.hue));
            }
            if !(0.0..=0.5).contains(&t.roles.rank_prior) {
                return fail(format!(
                    "type {name}: rank_prior {} out of 0..=0.5",
                    t.roles.rank_prior
                ));
            }
            validate_section(&t.brief, &format!("type {name} brief"))?;
        }

        let verbs = &self.ontology.verbs;
        let mut seen = std::collections::HashSet::new();
        for v in verbs {
            // Sentence-shaped: a lowercase hyphen-joined verb phrase, so any
            // triple reads as English ("X conflicts-with Y").
            if v.name.is_empty()
                || v.name.len() > 32
                || !v
                    .name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                || v.name.starts_with('-')
                || v.name.ends_with('-')
            {
                return fail(format!(
                    "verb {:?} must be a lowercase hyphen-joined word (sentence-shaped)",
                    v.name
                ));
            }
            if !seen.insert(v.name.clone()) {
                return fail(format!("duplicate verb {:?}", v.name));
            }
        }
        for (role, count) in [
            (
                "supersession",
                verbs.iter().filter(|v| v.roles.supersession).count(),
            ),
            (
                "contradiction",
                verbs.iter().filter(|v| v.roles.contradiction).count(),
            ),
        ] {
            if count != 1 {
                return fail(format!(
                    "exactly one verb must carry the {role} role (found {count}) — it is what keeps the graph active"
                ));
            }
        }
        if let Some(v) = verbs
            .iter()
            .find(|v| v.roles.supersession && v.roles.contradiction)
        {
            return fail(format!(
                "verb {:?} can't be both supersession and contradiction",
                v.name
            ));
        }

        let p = &self.policy;
        for (name, value) in [
            ("trust_created", p.trust_created),
            ("trust_confirmed", p.trust_confirmed),
            ("trust_approved", p.trust_approved),
            ("trust_approved_floor", p.trust_approved_floor),
            ("trust_floor", p.trust_floor),
            ("stale_trust", p.stale_trust),
        ] {
            if !(0.0..=1.0).contains(&value) {
                return fail(format!("policy.{name} {value} out of 0..=1"));
            }
        }
        if p.trust_floor > p.trust_created
            || p.trust_created > p.trust_confirmed
            || p.trust_confirmed > p.trust_approved
        {
            return fail(
                "trust anchors must order floor <= created <= confirmed <= approved".into(),
            );
        }
        for (name, days) in [
            ("episodic_window_days", p.episodic_window_days),
            ("volatile_window_days", p.volatile_window_days),
            ("approved_window_days", p.approved_window_days),
            ("decay_ttl_days", p.decay_ttl_days),
        ] {
            if !(1..=36_500).contains(&days) {
                return fail(format!("policy.{name} {days} out of 1..=36500"));
            }
        }
        for (name, value) in [
            ("duplicate_similarity", p.duplicate_similarity),
            ("conflict_suspect_similarity", p.conflict_suspect_similarity),
            ("warn_similarity", p.warn_similarity),
            ("nli_sweep_min_confidence", p.nli_sweep_min_confidence),
        ] {
            if !(0.0..=1.0).contains(&value) {
                return fail(format!("policy.{name} {value} out of 0..=1"));
            }
        }
        if p.conflict_suspect_similarity > p.duplicate_similarity {
            return fail(
                "conflict_suspect_similarity above duplicate_similarity leaves an unreachable suspect band".into(),
            );
        }

        let b = &self.brief;
        if !(1_000..=200_000).contains(&b.total_chars) {
            return fail(format!(
                "brief.total_chars {} out of 1000..=200000",
                b.total_chars
            ));
        }
        if b.home_reserve > b.total_chars {
            return fail("brief.home_reserve exceeds the total budget".into());
        }
        for (name, t) in [("tags", &b.tags), ("suspects", &b.suspects)] {
            if t.cap > 100 {
                return fail(format!("brief.{name}.cap {} out of 0..=100", t.cap));
            }
        }
        validate_section(&b.recent, "brief.recent")?;
        validate_section(&b.open, "brief.open")?;
        validate_section(&b.handoff, "brief.handoff")?;
        Ok(())
    }
}

fn validate_section(s: &BriefSection, what: &str) -> crate::Result<()> {
    if s.cap > 100 {
        return Err(crate::Error::Config(format!(
            "{what}: cap {} out of 0..=100",
            s.cap
        )));
    }
    if !(20..=2_000).contains(&s.excerpt) {
        return Err(crate::Error::Config(format!(
            "{what}: excerpt {} out of 20..=2000",
            s.excerpt
        )));
    }
    Ok(())
}
