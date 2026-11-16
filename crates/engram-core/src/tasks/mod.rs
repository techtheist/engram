//! Tasks: the assistant's working field — what it is doing, what it
//! parked, what it plans — kept BESIDE the graph, never in it. A task is not
//! knowledge: it never enters search, the suspect scan, trust, decay or the
//! eval corpora (Decision: tasks live next to the graph). It points at memory
//! through one-way `refs`; memory never points back.
//!
//! Shape: one level of groups (a task naming a `parent` makes that parent a
//! group, whose progress and closure derive from its children), five
//! statuses, and two optional lifecycle rules — `wake` (a parked task becomes
//! active) and `close` (an open task is done) — each one string: a version,
//! a date, or `node:<id>`. Rules settle at read time and on `set_version`;
//! there is no scheduler. Every automatic change says why in `auto`.

use serde::{Deserialize, Serialize};

use crate::engine::{ChangeEvent, Engine};
use crate::{Error, Result};

mod engine;

/// Where a task stands. The first three are open; done and dropped are
/// closed — archived, never deleted (hard delete is the pane's alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    /// Being worked on now or this cycle — what the brief lists.
    Active,
    /// Parked, with a reason or a wake rule.
    Delayed,
    /// Someday, no plan yet.
    Backlog,
    Done,
    /// Decided not to do it.
    Dropped,
}

impl TaskStatus {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "active" => Self::Active,
            "delayed" => Self::Delayed,
            "backlog" => Self::Backlog,
            "done" => Self::Done,
            "dropped" => Self::Dropped,
            other => {
                return Err(Error::Config(format!(
                    "unknown task status \"{other}\" — one of active, delayed, backlog, done, dropped"
                )));
            }
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Delayed => "delayed",
            Self::Backlog => "backlog",
            Self::Done => "done",
            Self::Dropped => "dropped",
        }
    }

    pub fn is_open(self) -> bool {
        matches!(self, Self::Active | Self::Delayed | Self::Backlog)
    }

    /// Listing order of the open statuses (closed ones sort after).
    fn rank(self) -> u8 {
        match self {
            Self::Active => 0,
            Self::Delayed => 1,
            Self::Backlog => 2,
            Self::Done => 3,
            Self::Dropped => 4,
        }
    }
}

/// One task as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    /// Markdown: the plan, a checklist, where work stopped. `progress`
    /// updates append dated lines here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub status: TaskStatus,
    /// The group this task belongs to (one level only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Memory node ids this task is about — one-way.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<String>,
    /// Canonical wake rule (see [`Rule`]); cleared when it fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<String>,
    /// Canonical close rule; cleared when it fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close: Option<String>,
    /// Manual order among siblings (pane drag; the brief's order).
    #[serde(default)]
    pub position: i64,
    /// The graph's working version when the task was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The working version when it closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_version: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<i64>,
    /// The session that created it, and the last one that changed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub touched_session: Option<String>,
    /// Who created it: pane | mcp | library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// The last automatic change and why ("closed: version 0.10.0 reached").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto: Option<String>,
}

/// One task to create. `parent` may name an existing task id or `$N` — the
/// N-th item (1-based) of the same batch, so a group and its children land
/// in one call.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewTask {
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub refs: Vec<String>,
    #[serde(default)]
    pub wake: Option<String>,
    #[serde(default)]
    pub close: Option<String>,
}

/// A partial update. Double options: `Some(None)` clears the field.
#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub body: Option<Option<String>>,
    pub status: Option<TaskStatus>,
    pub parent: Option<Option<String>>,
    pub refs: Option<Vec<String>>,
    pub wake: Option<Option<String>>,
    pub close: Option<Option<String>>,
    pub position: Option<i64>,
    /// One line appended to the body under today's date.
    pub progress: Option<String>,
}

/// The verdict of one `add_tasks` item.
#[derive(Debug, Clone, Serialize)]
pub struct TaskWrite {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub created: bool,
    /// An open task with (nearly) the same title — update it instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched: Option<TaskRef>,
    /// A close rule the daemon set on its own (release-shaped title).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_close: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskRef {
    pub id: String,
    pub title: String,
    pub status: TaskStatus,
}

/// What `list_tasks` filters on.
#[derive(Debug, Clone, Default)]
pub struct TaskFilter {
    /// Empty = the open statuses.
    pub statuses: Vec<TaskStatus>,
    pub parent: Option<String>,
    /// Only tasks whose refs include this node id.
    pub ref_id: Option<String>,
}

/// A task with its group context, as listings return it.
#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    #[serde(flatten)]
    pub task: Task,
    /// For a group: (closed children, all children).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<(usize, usize)>,
    /// Open task created under an older working version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carried_from: Option<String>,
    /// Sessions that briefed since this open task last changed, when at or
    /// over the idle threshold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_sessions: Option<usize>,
}

/// A parsed lifecycle rule. Canonical string forms: a version (`0.10.0` —
/// reached when the working version is at or past it), `>0.9.12` (strictly
/// past it), an ISO instant, or `node:<id>` (fires when that node is no
/// longer open: resolved, archived or deleted).
#[derive(Debug, Clone, PartialEq)]
pub enum Rule {
    Version(String),
    PastVersion(String),
    Date(i64),
    Node(String),
}

impl Rule {
    /// Parse what a caller wrote: also accepts relative future phrases
    /// ("tomorrow", "in 3 days", "next week") and past ones the shared
    /// time grammar reads.
    pub fn parse(spec: &str, now_ts: i64) -> Result<Self> {
        let t = spec.trim();
        if let Some(id) = t.strip_prefix("node:") {
            let id = id.trim();
            if id.is_empty() {
                return Err(rule_error(spec));
            }
            return Ok(Self::Node(id.to_string()));
        }
        if let Some(v) = t.strip_prefix('>') {
            let v = v.trim();
            if version_parts(v).is_some() {
                return Ok(Self::PastVersion(v.to_string()));
            }
            return Err(rule_error(spec));
        }
        if looks_like_version(t) {
            return Ok(Self::Version(t.to_string()));
        }
        parse_future(t, now_ts)
            .or_else(|| crate::timespec::parse_instant(t, now_ts))
            .map(Self::Date)
            .ok_or_else(|| rule_error(spec))
    }

    pub fn canonical(&self) -> String {
        match self {
            Self::Version(v) => v.clone(),
            Self::PastVersion(v) => format!(">{v}"),
            Self::Date(ts) => crate::timespec::iso_instant(*ts),
            Self::Node(id) => format!("node:{id}"),
        }
    }

    /// Plain words for `auto` and the pane.
    fn describe(&self) -> String {
        match self {
            Self::Version(v) => format!("version {v} reached"),
            Self::PastVersion(v) => format!("the working version moved past {v}"),
            Self::Date(ts) => format!("{} passed", crate::timespec::iso_instant(*ts)),
            Self::Node(id) => format!("node {id} closed"),
        }
    }
}

fn rule_error(spec: &str) -> Error {
    Error::Config(format!(
        "can't read the rule \"{spec}\" — use a version (\"0.10.0\", or \">0.9.12\" for \"once past it\"), a date (\"2026-10-20\", \"tomorrow\", \"in 3 days\", \"next week\") or \"node:<id>\""
    ))
}

/// `v0.10.0`, `0.9`, `26.7.23` — digits and dots with at least one dot,
/// optional leading v. Dates use dashes, so they never read as versions.
fn looks_like_version(s: &str) -> bool {
    let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
    s.contains('.') && version_parts(s).is_some()
}

/// Numeric components of a version; a pre-release tail on a component
/// ("0-rc1") keeps its leading digits.
fn version_parts(s: &str) -> Option<Vec<u64>> {
    let s = s.trim().strip_prefix(['v', 'V']).unwrap_or(s.trim());
    if s.is_empty() {
        return None;
    }
    s.split('.')
        .map(|p| {
            let digits: String = p.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect()
}

/// Compare two versions numerically when both parse; `None` otherwise.
pub(crate) fn version_cmp(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let (mut x, mut y) = (version_parts(a)?, version_parts(b)?);
    let n = x.len().max(y.len());
    x.resize(n, 0);
    y.resize(n, 0);
    Some(x.cmp(&y))
}

/// Forward relative phrases the search grammar has no use for.
fn parse_future(t: &str, now_ts: i64) -> Option<i64> {
    const DAY: i64 = 86_400;
    let unit = |w: &str| -> Option<i64> {
        Some(match w.trim_end_matches('s') {
            "minute" | "min" => 60,
            "hour" | "hr" => 3_600,
            "day" => DAY,
            "week" | "wk" => 7 * DAY,
            "month" | "mon" => 30 * DAY,
            "year" | "yr" => 365 * DAY,
            _ => return None,
        })
    };
    let lower = t.to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let today = now_ts - now_ts.rem_euclid(DAY);
    match words.as_slice() {
        ["tomorrow"] => Some(today + DAY),
        ["next", u] => Some(now_ts + unit(u)?),
        ["in", "a" | "an", u] => Some(now_ts + unit(u)?),
        ["in", n, u] => Some(now_ts + n.parse::<i64>().ok()? * unit(u)?),
        _ => None,
    }
}

/// A title that names a release step for a specific version ("publish
/// 0.9.12", "Release v1.2", "tag 0.9.12 and push") — returns that version.
/// Such a task closes on its own once the project moves past it.
fn release_version(title: &str) -> Option<String> {
    const VERBS: [&str; 5] = ["publish", "release", "tag", "ship", "cut"];
    let lower = title.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':' | '(' | ')' | '—'))
        .filter(|w| !w.is_empty())
        .map(|w| w.trim_end_matches(['.', '!', '?']))
        .collect();
    // The verb must name the version itself ("publish 0.9.12", "release
    // v1.2"); "ship tasks in 0.10.0" is a feature landing in a release, not
    // the release step.
    words.windows(2).find_map(|pair| {
        let verb = VERBS
            .iter()
            .any(|v| pair[0] == *v || pair[0] == format!("{v}s") || pair[0] == format!("{v}ed"));
        (verb && looks_like_version(pair[1]))
            .then(|| pair[1].strip_prefix('v').unwrap_or(pair[1]).to_string())
    })
}

/// Word-set similarity of two titles (Jaccard over lowercased alphanumeric
/// words). Deterministic and model-free: the duplicate guard only has to
/// catch a new session re-adding the same work, and titles are short.
fn title_similarity(a: &str, b: &str) -> f64 {
    let words = |s: &str| -> std::collections::BTreeSet<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric() && c != '.')
            .map(|w| w.trim_matches('.').to_string())
            .filter(|w| !w.is_empty())
            .collect()
    };
    let (x, y) = (words(a), words(b));
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    x.intersection(&y).count() as f64 / x.union(&y).count() as f64
}

/// What a task call answers when the graph has tasks turned off: what
/// happened, what to do instead, and not to lobby for it.
pub const TASKS_OFF: &str = "tasks are turned off for this graph — the user's choice (Settings → Graph settings → Tasks in the pane). Don't keep a task list here: work without one, and use an open Intent tagged `handoff` for anything the next session must see first. Don't ask to turn tasks back on unless the user brings it up.";

/// At or above this word-set similarity, a new task matches an open one.
const DUPLICATE_SIMILARITY: f64 = 0.75;
/// Longest title accepted (the brief prints titles whole).
const MAX_TITLE: usize = 300;

/// Sort: open statuses in rank order, then manual position, then age;
/// closed tasks newest-closed first.
fn listing_order(a: &Task, b: &Task) -> std::cmp::Ordering {
    match (a.status.is_open(), b.status.is_open()) {
        (true, true) => (a.status.rank(), a.position, a.created_at, &a.id).cmp(&(
            b.status.rank(),
            b.position,
            b.created_at,
            &b.id,
        )),
        (false, false) => (b.closed_at, &b.id).cmp(&(a.closed_at, &a.id)),
        (open_a, _) => {
            if open_a {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        }
    }
}

/// Distinct sessions (other than `current`) that briefed after `ts`.
fn sessions_after(briefs: &[(i64, String)], ts: i64, current: Option<&str>) -> usize {
    briefs
        .iter()
        .filter(|(at, s)| *at > ts && Some(s.as_str()) != current)
        .map(|(_, s)| s.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

/// An item refusal as the caller should read it — the message alone, no
/// `config:` / `not found:` kind prefix.
fn plain_message(e: Error) -> String {
    match e {
        Error::Config(s) => s,
        Error::NotFound(s) => s,
        e => e.to_string(),
    }
}

fn clean_title(title: &str) -> Result<String> {
    let t = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        return Err(Error::Config("a task needs a title".into()));
    }
    if t.chars().count() > MAX_TITLE {
        return Err(Error::Config(format!(
            "task titles stay under {MAX_TITLE} characters — detail goes in the body"
        )));
    }
    Ok(t)
}

/// A caller's rule → its canonical stored form; blank = no rule.
fn canonical_rule(spec: Option<&str>, now: i64) -> Result<Option<String>> {
    match spec.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => Ok(Some(Rule::parse(s, now)?.canonical())),
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn rules_parse_to_canonical_forms() {
        let now = 1_760_000_000;
        assert_eq!(
            Rule::parse("0.10.0", now).unwrap(),
            Rule::Version("0.10.0".into())
        );
        assert_eq!(
            Rule::parse("v1.2", now).unwrap(),
            Rule::Version("v1.2".into())
        );
        assert_eq!(
            Rule::parse(">0.9.12", now).unwrap(),
            Rule::PastVersion("0.9.12".into())
        );
        assert_eq!(
            Rule::parse("node:00abc", now).unwrap(),
            Rule::Node("00abc".into())
        );
        assert_eq!(
            Rule::parse("in 3 days", now).unwrap(),
            Rule::Date(now + 3 * 86_400)
        );
        assert!(matches!(
            Rule::parse("2026-10-20", now).unwrap(),
            Rule::Date(_)
        ));
        assert!(matches!(
            Rule::parse("tomorrow", now).unwrap(),
            Rule::Date(_)
        ));
        assert!(Rule::parse("whenever", now).is_err());
        assert!(Rule::parse(">soon", now).is_err());
    }

    #[test]
    fn versions_compare_numerically() {
        use std::cmp::Ordering::*;
        assert_eq!(version_cmp("0.10.0", "0.9.12"), Some(Greater));
        assert_eq!(version_cmp("v0.9.12", "0.9.12"), Some(Equal));
        assert_eq!(version_cmp("1.0", "1.0.0"), Some(Equal));
        assert_eq!(version_cmp("next", "1.0"), None);
    }

    #[test]
    fn release_titles_name_their_version() {
        assert_eq!(release_version("Publish 0.9.12"), Some("0.9.12".into()));
        assert_eq!(
            release_version("tag v1.2.0 and push the draft"),
            Some("1.2.0".into())
        );
        assert_eq!(release_version("Release notes cleanup"), None);
        assert_eq!(release_version("Fix the 0.9.12 regression"), None);
        assert_eq!(release_version("Ship tasks in 0.10.0"), None);
        assert_eq!(
            release_version("Cut 0.10.0: tag and push"),
            Some("0.10.0".into())
        );
    }

    #[test]
    fn title_similarity_catches_rewordings_only() {
        assert!(title_similarity("Fix flaky e2e test", "fix the flaky e2e test") >= 0.75);
        assert!(title_similarity("Fix flaky e2e test", "Write the release notes") < 0.2);
    }
}
