//! The shipped ontology presets and the default ontology.

use super::*;

pub(crate) fn tdef(
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

pub(crate) fn vdef(name: &str, reads_as: &str, roles: VerbRoles) -> VerbDef {
    VerbDef {
        name: name.into(),
        reads_as: reads_as.into(),
        roles,
    }
}

pub(crate) fn hidden_brief() -> BriefSection {
    BriefSection {
        show: false,
        cap: 8,
        excerpt: 140,
    }
}

pub(super) fn shown_brief(cap: usize, excerpt: usize) -> BriefSection {
    BriefSection {
        show: true,
        cap,
        excerpt,
    }
}

/// One shipped ontology template: a complete, valid
/// config a user can start from. Applying one is just `PUT /config` with
/// its document — the pane's "start from preset" gesture.
#[derive(Debug, Clone, Serialize)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub description: String,
    pub config: GraphConfig,
}

/// The curated preset shelf. `engram` is the default 9-type product-building
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
                tdef(
                    "Retraction",
                    15,
                    "withdrawn — this no longer holds",
                    Durability::Stable,
                    TypeRoles {
                        tombstone: true,
                        highlight: false,
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

    let general = GraphConfig {
        ontology: OntologyConfig {
            preset: "general".into(),
            types: vec![
                tdef(
                    "Fact",
                    217,
                    "something true about this world",
                    Durability::Stable,
                    TypeRoles::plain(0.03),
                    shown_brief(8, 140),
                ),
                tdef(
                    "Note",
                    292,
                    "anything worth keeping",
                    Durability::Episodic,
                    TypeRoles::plain(0.0),
                    shown_brief(10, 140),
                ),
                tdef(
                    "Idea",
                    142,
                    "a possibility to explore",
                    Durability::Episodic,
                    TypeRoles::plain(0.02),
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
                tdef(
                    "Reference",
                    215,
                    "an external subject or source",
                    Durability::Stable,
                    TypeRoles {
                        anchor: true,
                        versioned: false,
                        ..TypeRoles::plain(0.0)
                    },
                    hidden_brief(),
                ),
                tdef(
                    "Tombstone",
                    15,
                    "deliberately removed — don't resurrect it",
                    Durability::Stable,
                    TypeRoles {
                        tombstone: true,
                        highlight: false,
                        ..TypeRoles::plain(0.0)
                    },
                    hidden_brief(),
                ),
            ],
            verbs: vec![
                vdef("about", "Note about Reference", VerbRoles::default()),
                vdef(
                    "because",
                    "Fact because Note",
                    VerbRoles {
                        reason: true,
                        ..VerbRoles::default()
                    },
                ),
                vdef(
                    "answers",
                    "Note answers Task",
                    VerbRoles {
                        answer: true,
                        ..VerbRoles::default()
                    },
                ),
                vdef("builds-on", "Idea builds-on Idea", VerbRoles::default()),
                vdef(
                    "replaces",
                    "Fact replaces Fact",
                    VerbRoles {
                        supersession: true,
                        ..VerbRoles::default()
                    },
                ),
                vdef(
                    "conflicts-with",
                    "Note conflicts-with Fact",
                    VerbRoles {
                        contradiction: true,
                        ..VerbRoles::default()
                    },
                ),
                vdef(
                    "needs",
                    "Task needs Idea",
                    VerbRoles {
                        dependency: true,
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
                          problems and their resolutions, insights, intents, code anchors, \
                          tombstones for removed knowledge."
                .into(),
            config: GraphConfig::default(),
        },
        Preset {
            id: "research".into(),
            name: "Research".into(),
            description: "For investigation-shaped work: claims with evidence, open questions, \
                          findings, sources, methods, retractions — refutes carries the \
                          contradiction role."
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
        Preset {
            id: "general".into(),
            name: "General".into(),
            description: "An open experimental set for saving almost anything: facts, notes, \
                          ideas, tasks, references, tombstones — designed to pair with custom \
                          fields."
                .into(),
            config: general,
        },
    ]
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
                        ..TypeRoles::plain(0.025)
                    },
                    shown(8, 140),
                ),
                t(
                    "Decision",
                    217,
                    "we chose this, for a reason",
                    Durability::Stable,
                    TypeRoles::plain(0.02),
                    shown(7, 80),
                ),
                t(
                    "Caution",
                    38,
                    "watch out — this bites",
                    Durability::Stable,
                    TypeRoles::plain(0.025),
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
                    TypeRoles::plain(0.02),
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
                    hidden.clone(),
                ),
                t(
                    "Tombstone",
                    15,
                    "deliberately removed — don't resurrect it",
                    Durability::Stable,
                    TypeRoles {
                        tombstone: true,
                        highlight: false,
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
// validation — the hard invariants
// ---------------------------------------------------------------------------
