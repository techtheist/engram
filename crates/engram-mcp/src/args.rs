//! Tool argument schemas.

use crate::*;

//
// Scoped tools share an optional `project` selector: omitted = current; a
// name, id or directory inside a repo = that project (longest root wins);
// "home" = the user graph; "all" = every project, reads only.

/// Numbers the way models send them: a JSON number or numeric string
/// (`"15"`, `15.0`) is accepted, an empty string is absent, anything else is
/// refused with the field's message. The schema still advertises a number.
pub(crate) mod lenient {
    use serde::de::Error;
    use serde::{Deserialize, Deserializer};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Int(i64),
        Float(f64),
        Str(String),
    }

    pub trait Num: Sized {
        const KIND: &'static str;
        fn from_i64(v: i64) -> Option<Self>;
        fn from_f64(v: f64) -> Option<Self> {
            (v.is_finite() && v.fract() == 0.0 && v.abs() < 9.0e15)
                .then(|| Self::from_i64(v as i64))
                .flatten()
        }
    }

    macro_rules! unsigned {
        ($($t:ty),*) => {$(
            impl Num for $t {
                const KIND: &'static str = "a non-negative whole number";
                fn from_i64(v: i64) -> Option<Self> {
                    <$t>::try_from(v).ok()
                }
            }
        )*};
    }
    unsigned!(usize, u64, u32);

    impl Num for i64 {
        const KIND: &'static str = "a whole number";
        fn from_i64(v: i64) -> Option<Self> {
            Some(v)
        }
    }

    impl Num for f64 {
        const KIND: &'static str = "a number";
        fn from_i64(v: i64) -> Option<Self> {
            Some(v as f64)
        }
        fn from_f64(v: f64) -> Option<Self> {
            v.is_finite().then_some(v)
        }
    }

    fn parse<T: Num, E: Error>(raw: Raw) -> Result<Option<T>, E> {
        let refuse = |got: &str| E::custom(format!("expected {}, got {got}", T::KIND));
        match raw {
            Raw::Int(v) => T::from_i64(v)
                .map(Some)
                .ok_or_else(|| refuse(&v.to_string())),
            Raw::Float(v) => T::from_f64(v)
                .map(Some)
                .ok_or_else(|| refuse(&v.to_string())),
            Raw::Str(s) => {
                let t = s.trim();
                if t.is_empty() {
                    return Ok(None);
                }
                t.parse::<i64>()
                    .ok()
                    .and_then(T::from_i64)
                    .or_else(|| t.parse::<f64>().ok().and_then(T::from_f64))
                    .map(Some)
                    .ok_or_else(|| refuse(&format!("{s:?}")))
            }
        }
    }

    pub fn opt<'de, D: Deserializer<'de>, T: Num>(d: D) -> Result<Option<T>, D::Error> {
        match Option::<Raw>::deserialize(d)? {
            None => Ok(None),
            Some(raw) => parse(raw),
        }
    }

    pub fn req<'de, D: Deserializer<'de>, T: Num>(d: D) -> Result<T, D::Error> {
        parse(Raw::deserialize(d)?)?
            .ok_or_else(|| D::Error::custom(format!("expected {}, got an empty string", T::KIND)))
    }
}

#[derive(Deserialize, JsonSchema, Default)]
pub(crate) struct SearchArgs {
    pub(crate) query: String,
    #[serde(default)]
    pub(crate) types: Vec<String>,
    #[serde(default, deserialize_with = "lenient::opt")]
    pub(crate) limit: Option<usize>,
    /// Bitemporal search: aim `after`/`before` at a date-kind CUSTOM
    /// field instead of created_at — the event clock for historic imports.
    /// A single field name ("event_date") matches values inside the window;
    /// a "from..to" pair ("effective_from..effective_to") matches nodes
    /// whose validity span OVERLAPS the window. Needs after and/or before;
    /// declared fields: describe_ontology.
    #[serde(default)]
    pub(crate) date_field: Option<String>,
    /// Progressive disclosure: "compact" = id/type/title/score/trust only
    /// (cheapest — start here when scanning), "snippet" (default) = compact
    /// plus matched snippet, 1-hop neighbors and (top 3 hits) one found
    /// bridge — a note a bridge note ties it to with no edge, "full" = snippet plus the
    /// complete body of every hit. Expand on demand instead of paying for
    /// depth up front.
    #[serde(default)]
    pub(crate) detail: Option<String>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home"; "all" = every project + home with provenance
    /// (reads only).
    #[serde(default)]
    pub(crate) project: Option<String>,
    /// "auto" (default) = curated memory, with session history appearing as
    /// a separate labeled section only when the confidence verdict says the
    /// answer is likely not in memory; "memory" = curated only;
    /// "history" = session history only.
    #[serde(default)]
    pub(crate) scope: Option<String>,
    /// Only knowledge captured at or after this instant. Takes a day
    /// ("2026-08-14"), an ISO instant ("2026-08-14T09:15:32Z"), or a relative
    /// expression the daemon resolves ("today", "yesterday", "last week",
    /// "last 3 days", "2 hours ago", "a month ago", "this year"). Relative
    /// expressions mean the START of the span, so after: "last week" reads
    /// "since a week ago".
    #[serde(default)]
    pub(crate) after: Option<String>,
    /// Only knowledge captured strictly before this instant — same grammar as
    /// `after`. Pair them for a bounded window.
    #[serde(default)]
    pub(crate) before: Option<String>,
    /// Only knowledge captured while this working version was current (e.g.
    /// "0.8.4"), resolved from the graph's recorded `set_version` switches.
    /// Combines with after/before, which narrow it further.
    #[serde(default)]
    pub(crate) during_version: Option<String>,
    /// "relevance" (default) = score order, the ranking the confidence verdict
    /// is defined against; "chronological" = oldest first, for reading how
    /// something developed; "recent" = newest first, for "what is the CURRENT
    /// value of X". Ordering is applied after every cut, so it re-orders the
    /// delivered set without changing which hits were delivered.
    #[serde(default)]
    pub(crate) order: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ExpandHistoryArgs {
    /// The session handle a history hit carried.
    pub(crate) session: String,
    /// The turn to center on.
    #[serde(deserialize_with = "lenient::req")]
    pub(crate) turn: u64,
    /// Messages of context on each side (default 4, max 25).
    #[serde(default, deserialize_with = "lenient::opt")]
    pub(crate) window: Option<u64>,
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema, Default)]
pub(crate) struct ListSessionsArgs {
    /// Max sessions returned, newest first (default 20, max 100).
    #[serde(default, deserialize_with = "lenient::opt")]
    pub(crate) limit: Option<usize>,
    /// Only sessions of one harness ("claude-code", "codex", "bob", …).
    #[serde(default)]
    pub(crate) harness: Option<String>,
    /// Only sessions overlapping the window starting here — same grammar as
    /// search's `after` (a day, an ISO instant, or "last week" / "yesterday" /
    /// "3 days ago"). A session that began earlier and ran into the window
    /// counts as inside it.
    #[serde(default)]
    pub(crate) after: Option<String>,
    /// Only sessions that had started before this instant — same grammar.
    #[serde(default)]
    pub(crate) before: Option<String>,
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct IdArg {
    pub(crate) id: String,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ProjectArg {
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct AuditArgs {
    /// Max rows to return (default 20, newest first).
    #[serde(default, deserialize_with = "lenient::opt")]
    pub(crate) limit: Option<usize>,
    /// Keyset cursor: only rows with seq strictly below this (page with the
    /// last row's seq).
    #[serde(default, deserialize_with = "lenient::opt")]
    pub(crate) before: Option<i64>,
    /// Restrict to one node/edge id's history.
    #[serde(default)]
    pub(crate) entity_id: Option<String>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct GetNodeArgs {
    pub(crate) id: String,
    /// Levels of parent hierarchy to include (nodes this one points at), 0-3.
    #[serde(default, deserialize_with = "lenient::opt")]
    pub(crate) parents: Option<usize>,
    /// Levels of child hierarchy to include (nodes pointing at this one), 0-3.
    #[serde(default, deserialize_with = "lenient::opt")]
    pub(crate) children: Option<usize>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct TraverseArgs {
    pub(crate) from: String,
    #[serde(default)]
    pub(crate) edge_types: Vec<String>,
    #[serde(default, deserialize_with = "lenient::opt")]
    pub(crate) depth: Option<usize>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

// `inline`: otherwise batch items collapse to a bare `$ref` that some models
// don't chase (issue #9). `deny_unknown_fields`: an inline `links` must fail.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub(crate) struct AddNoteArgs {
    /// Node type — the default ontology's 9: Decision | Principle | Caution |
    /// Problem | Resolution | Insight | Intent | Anchor | Tombstone (this
    /// graph's actual set: describe_ontology).
    #[serde(rename = "type")]
    pub(crate) node_type: String,
    /// One-sentence summary, the claim itself (required; detail goes in body).
    pub(crate) title: String,
    /// Custom field values, when this graph declares custom fields —
    /// {"name": value} against the definitions describe_ontology lists
    /// (kinds, required flags, enum vocabularies). Unknown names and wrong
    /// kinds are refused with the full roster.
    #[serde(default)]
    pub(crate) fields: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    pub(crate) body: Option<String>,
    #[serde(default)]
    pub(crate) durability: Option<String>,
    #[serde(default)]
    pub(crate) session_id: Option<String>,
    /// Captured-at version override (version tracking): omit to auto-stamp
    /// the graph's current working version on version-bound types.
    #[serde(default)]
    pub(crate) version: Option<String>,
    #[serde(default)]
    pub(crate) code_refs: Vec<String>,
    /// Free-form slice labels (kebab-cased on write). Reuse the recent tags
    /// listed in the brief before inventing new ones; new tags are created
    /// implicitly.
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    /// The knowledge's ORIGINAL date, for digesting historical material:
    /// "YYYY-MM-DD" (or unix seconds). Omit for live capture — then the note
    /// is dated now. Never use for fresh knowledge.
    #[serde(default)]
    pub(crate) created_at: Option<String>,
    /// Omit = current project; a name, id, or directory writes into THAT
    /// project's graph (deliberate cross-project capture); "home" = the
    /// user-level graph.
    /// "all" is refused — a fanned-out write is replication.
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinkArgs {
    /// Subject node id — the edge reads "from <verb> to".
    pub(crate) from: String,
    /// Object node id.
    pub(crate) to: String,
    /// Edge verb (required) — one of: about | because | answers | builds-on |
    /// replaces | conflicts-with | needs.
    #[serde(rename = "type")]
    pub(crate) edge_type: String,
    #[serde(default)]
    pub(crate) note: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt")]
    pub(crate) confidence: Option<f64>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project (both endpoints must live there — edges never cross
    /// graphs); "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

// `inline` for the same wire-visibility reason as AddNoteArgs (update_nodes'
// items would otherwise hide behind a $ref).
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub(crate) struct UpdateArgs {
    pub(crate) id: String,
    /// Reclassify the node (one of the 9 canonical types).
    #[serde(default, rename = "type")]
    pub(crate) node_type: Option<String>,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) body: Option<String>,
    #[serde(default)]
    pub(crate) durability: Option<String>,
    #[serde(default)]
    pub(crate) status: Option<String>,
    #[serde(default)]
    pub(crate) code_refs: Option<Vec<String>>,
    /// Replaces the node's tag list when set (kebab-cased on write).
    #[serde(default)]
    pub(crate) tags: Option<Vec<String>>,
    /// Set or correct the node's captured-at version (version tracking).
    #[serde(default)]
    pub(crate) version: Option<String>,
    /// Custom field values, MERGED into the node's existing ones: present
    /// keys overwrite, a null value deletes that key, absent keys survive.
    /// Definitions (kinds, required, enum vocabularies): describe_ontology.
    #[serde(default)]
    pub(crate) fields: Option<serde_json::Map<String, serde_json::Value>>,
    /// true = mark reviewed in a sleep sweep (hides its subject or island from the
    /// review queue until the note changes; trust untouched); false clears.
    #[serde(default)]
    pub(crate) reviewed: Option<bool>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MergeArgs {
    /// The node that lives on and receives the union.
    pub(crate) survivor: String,
    /// Nodes to fold into the survivor — each ends archived behind a
    /// replaces edge, its live edges moved to the survivor.
    pub(crate) victims: Vec<String>,
    /// Merged title, composed from the parts (omit = survivor's stands).
    #[serde(default)]
    pub(crate) title: Option<String>,
    /// Merged body, composed from the parts (omit = survivor's stands —
    /// the victims' bodies are NOT appended automatically).
    #[serde(default)]
    pub(crate) body: Option<String>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AddNotesArgs {
    /// Notes to create; each item takes the same fields as add_note (type and
    /// title required). Items carry NO links — create the notes first, then
    /// `link` the returned ids in a second pass.
    pub(crate) notes: Vec<AddNoteArgs>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateNodesArgs {
    /// Patches to apply; each item takes the same fields as update_node.
    pub(crate) updates: Vec<UpdateArgs>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ListNodesArgs {
    /// Filter to these node types (default: all 9).
    #[serde(default)]
    pub(crate) types: Vec<String>,
    /// Filter Problems/Intents by status: open | resolved | obsolete.
    #[serde(default)]
    pub(crate) status: Option<String>,
    /// Only nodes carrying this tag.
    #[serde(default)]
    pub(crate) tag: Option<String>,
    /// Ordering: "recent" (default, newest first), "most-connected" or
    /// "least-connected" (by live edge count — the least-connected end is
    /// where unreachable knowledge hides), "crowded" (most close neighbours
    /// first; each node carries `crowding`).
    #[serde(default)]
    pub(crate) sort: Option<String>,
    /// The sleep queues: "conflicts" = standing contradiction edges and
    /// pending suspects; "review" = subjects of close notes, most worth
    /// consolidating first (limit counts subjects, default 1; types/tag
    /// filter members); "islands" = notes no edge and no tag reaches, with
    /// their nearest notes (default limit 20). Default "nodes".
    #[serde(default)]
    pub(crate) view: Option<String>,
    /// A note id: return its neighbourhood instead of a list — nearest
    /// notes by embedding and subject-mates (shared anchor/tag/code ref).
    #[serde(default)]
    pub(crate) around: Option<String>,
    /// With view "review" / "islands": also show subjects or islands
    /// already marked reviewed. Default false.
    #[serde(default)]
    pub(crate) include_reviewed: Option<bool>,
    /// Also return archived (superseded) generations. Default false.
    #[serde(default)]
    pub(crate) include_archived: Option<bool>,
    /// true = only user-pinned (constant-trust) nodes; false = only unpinned.
    #[serde(default)]
    pub(crate) pinned: Option<bool>,
    /// Page size (default 30, max 200).
    #[serde(default, deserialize_with = "lenient::opt")]
    pub(crate) limit: Option<usize>,
    /// Skip this many (after filtering, newest first).
    #[serde(default, deserialize_with = "lenient::opt")]
    pub(crate) offset: Option<usize>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ApproveArgs {
    pub(crate) id: String,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct CheckClaimArgs {
    /// The claim to verify, as one declarative sentence.
    pub(crate) claim: String,
    /// How many nearby nodes to judge (default 8, max 16).
    #[serde(default, deserialize_with = "lenient::opt")]
    pub(crate) limit: Option<usize>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home"; "all" = judge across every project + home with
    /// provenance.
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ResolveSuspectArgs {
    /// The suspect id (from the brief's "Suspected conflicts" section or list_suspects).
    pub(crate) id: String,
    /// "conflict" | "replaces" | "dismiss"
    pub(crate) verdict: String,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AddTasksArgs {
    /// Tasks to create, in order — a later item's `parent: "$1"` names the
    /// first item of this call.
    pub(crate) tasks: Vec<AddTaskItem>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(inline)]
pub(crate) struct AddTaskItem {
    /// One line: the piece of work (the brief prints it whole).
    pub(crate) title: String,
    /// Markdown — the plan, a checklist of steps, open questions.
    #[serde(default)]
    pub(crate) body: Option<String>,
    /// active (default) | delayed | backlog.
    #[serde(default)]
    pub(crate) status: Option<String>,
    /// A task id, or "$N" (1-based) = an earlier item of this call. Groups
    /// are one level deep.
    #[serde(default)]
    pub(crate) parent: Option<String>,
    /// Memory node ids this task is about (one-way: notes never point back).
    #[serde(default)]
    pub(crate) refs: Vec<String>,
    /// When a delayed/backlog task becomes active: "0.10.1", ">0.10.0",
    /// "2026-10-20", "in 3 days", "next week", or "node:<id>".
    #[serde(default)]
    pub(crate) wake: Option<String>,
    /// When an open task is done on its own — same forms as `wake`.
    #[serde(default)]
    pub(crate) close: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateTaskArgs {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    /// Replaces the body ("" clears). To add a line, use `progress`.
    #[serde(default)]
    pub(crate) body: Option<String>,
    /// active | delayed | backlog | done | dropped.
    #[serde(default)]
    pub(crate) status: Option<String>,
    /// Move into a group (a top-level task id); "" = ungroup.
    #[serde(default)]
    pub(crate) parent: Option<String>,
    /// Replaces the refs list.
    #[serde(default)]
    pub(crate) refs: Option<Vec<String>>,
    /// Wake rule ("" clears).
    #[serde(default)]
    pub(crate) wake: Option<String>,
    /// Close rule ("" clears).
    #[serde(default)]
    pub(crate) close: Option<String>,
    /// Manual order among siblings (lower first).
    #[serde(default, deserialize_with = "lenient::opt")]
    pub(crate) position: Option<i64>,
    /// One line appended to the body under today's date — where you stopped.
    #[serde(default)]
    pub(crate) progress: Option<String>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListTasksArgs {
    /// Omit = open (active, delayed, backlog). Comma-separated statuses,
    /// "open", "closed" or "all".
    #[serde(default)]
    pub(crate) status: Option<String>,
    /// Only this group's tasks.
    #[serde(default)]
    pub(crate) parent: Option<String>,
    /// Only tasks whose refs include this node id.
    #[serde(default, rename = "ref")]
    pub(crate) ref_id: Option<String>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ListOpenArgs {
    #[serde(default)]
    pub(crate) types: Vec<String>,
    #[serde(default)]
    pub(crate) include_conflicts: Option<bool>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct BriefArgs {
    /// Character budget for the digest (default ~16000, about 4k tokens).
    #[serde(default, deserialize_with = "lenient::opt")]
    pub(crate) max_chars: Option<usize>,
    /// Omit = this session's project plus the home-graph section. A name, an
    /// id, an absolute path inside a registered root, or "home" REBINDS this
    /// session to that project and briefs it — session-scoped, and a refused
    /// selector (unknowns are refused with the known roster) leaves the
    /// binding untouched.
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct SetVersionArgs {
    /// The new current version ("v0.7.0", "26.7.23", …); null/omit clears it.
    #[serde(default)]
    pub(crate) version: Option<String>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DescribeOntologyArgs {
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct UpdateEdgeArgs {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) status: Option<String>,
    #[serde(default)]
    pub(crate) note: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt")]
    pub(crate) confidence: Option<f64>,
    /// Omit = current project; a name, id, or the project's directory =
    /// that project; "home".
    #[serde(default)]
    pub(crate) project: Option<String>,
}
