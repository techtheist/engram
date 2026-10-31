//! Tasks (0.10.0): the assistant's working field — what it is doing, what it
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
            let digits: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
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

impl Engine {
    /// Whether this graph keeps a task list (config `tasks.enabled`).
    pub fn tasks_enabled(&self) -> bool {
        self.store().config().tasks.enabled
    }

    /// Refuse a task call on a graph with tasks off.
    fn require_tasks(&self) -> Result<()> {
        if self.tasks_enabled() {
            Ok(())
        } else {
            Err(Error::Config(TASKS_OFF.into()))
        }
    }

    /// Every task, rules settled first. Empty while tasks are off (callers
    /// that only report — set_version, the brief — stay quiet).
    pub fn tasks(&self) -> Result<Vec<Task>> {
        if !self.tasks_enabled() {
            return Ok(Vec::new());
        }
        self.settle_tasks()?;
        let mut all = self.store().all_tasks()?;
        all.sort_by(listing_order);
        Ok(all)
    }

    pub fn get_task(&self, id: &str) -> Result<Option<Task>> {
        self.store().get_task(id)
    }

    /// Filtered listing with group progress and carry/idle marks.
    pub fn list_tasks(&self, filter: &TaskFilter) -> Result<Vec<TaskView>> {
        self.require_tasks()?;
        let all = self.tasks()?;
        let idle_after = self.store().config().brief.tasks.idle_sessions;
        let current = self.current_version()?;
        let briefs = self.brief_sessions()?;
        let statuses: &[TaskStatus] = if filter.statuses.is_empty() {
            &[TaskStatus::Active, TaskStatus::Delayed, TaskStatus::Backlog]
        } else {
            &filter.statuses
        };
        let views = all
            .iter()
            .filter(|t| statuses.contains(&t.status))
            .filter(|t| {
                filter
                    .parent
                    .as_deref()
                    .is_none_or(|p| t.parent.as_deref() == Some(p))
            })
            .filter(|t| {
                filter
                    .ref_id
                    .as_deref()
                    .is_none_or(|r| t.refs.iter().any(|x| x == r))
            })
            .map(|t| self.view_of(t, &all, current.as_deref(), &briefs, idle_after))
            .collect();
        Ok(views)
    }

    /// One task's listing view, against the full set (for group progress).
    fn view_of(
        &self,
        t: &Task,
        all: &[Task],
        current: Option<&str>,
        briefs: &[(i64, String)],
        idle_after: usize,
    ) -> TaskView {
        let children: Vec<&Task> = all
            .iter()
            .filter(|c| c.parent.as_deref() == Some(t.id.as_str()))
            .collect();
        let progress = (!children.is_empty()).then(|| {
            (
                children.iter().filter(|c| !c.status.is_open()).count(),
                children.len(),
            )
        });
        let carried_from = match (t.status.is_open(), t.version.as_deref(), current) {
            (true, Some(v), Some(cur)) if v != cur => Some(v.to_string()),
            _ => None,
        };
        let idle = if t.status.is_open() && idle_after > 0 {
            let last = children
                .iter()
                .map(|c| c.updated_at)
                .chain([t.updated_at])
                .max()
                .unwrap_or(t.updated_at);
            let n = sessions_after(briefs, last, self.audit_session().as_deref());
            (n >= idle_after).then_some(n)
        } else {
            None
        };
        TaskView {
            task: t.clone(),
            progress,
            carried_from,
            idle_sessions: idle,
        }
    }

    /// `(ts, session)` of every journaled brief — the session clock idle
    /// marks count on.
    fn brief_sessions(&self) -> Result<Vec<(i64, String)>> {
        let rows = self.store().audit_rows(&crate::types::AuditQuery {
            action: Some("brief_served".into()),
            ..Default::default()
        })?;
        Ok(rows
            .into_iter()
            .filter_map(|r| r.session_id.filter(|s| !s.is_empty()).map(|s| (r.ts, s)))
            .collect())
    }

    /// Create tasks; one bad item never blocks the rest.
    pub fn add_tasks(&self, items: Vec<NewTask>) -> Result<Vec<TaskWrite>> {
        self.require_tasks()?;
        self.settle_tasks()?;
        let now = crate::store::now();
        let version = self.current_version()?;
        let mut out: Vec<TaskWrite> = Vec::with_capacity(items.len());
        let mut changed = false;
        for item in items {
            let res = self.add_one_task(item, &out, now, version.as_deref());
            match res {
                Ok(w) => {
                    changed |= w.created;
                    out.push(w);
                }
                Err(e) => out.push(TaskWrite {
                    id: None,
                    created: false,
                    matched: None,
                    auto_close: None,
                    error: Some(plain_message(e)),
                }),
            }
        }
        if changed {
            self.notify_tasks();
        }
        Ok(out)
    }

    fn add_one_task(
        &self,
        item: NewTask,
        earlier: &[TaskWrite],
        now: i64,
        version: Option<&str>,
    ) -> Result<TaskWrite> {
        let title = clean_title(&item.title)?;
        let status = match item.status.as_deref() {
            Some(s) => TaskStatus::parse(s)?,
            None => TaskStatus::Active,
        };
        let parent = match item.parent.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(p) => Some(self.resolve_parent(p, earlier)?),
        };
        if let Some(p) = &parent {
            self.check_parent(p, None)?;
        }
        let refs = self.check_refs(&item.refs)?;
        let wake = canonical_rule(item.wake.as_deref(), now)?;
        let (close, auto_close) = match canonical_rule(item.close.as_deref(), now)? {
            Some(c) => (Some(c), None),
            None => match release_version(&title) {
                Some(v) => {
                    let rule = format!(">{v}");
                    (Some(rule.clone()), Some(rule))
                }
                None => (None, None),
            },
        };

        let all = self.store().all_tasks()?;
        if status.is_open()
            && let Some(dupe) = all
                .iter()
                .filter(|t| t.status.is_open())
                .map(|t| (t, title_similarity(&t.title, &title)))
                .filter(|(_, s)| *s >= DUPLICATE_SIMILARITY)
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(t, _)| t)
        {
            return Ok(TaskWrite {
                id: None,
                created: false,
                matched: Some(TaskRef {
                    id: dupe.id.clone(),
                    title: dupe.title.clone(),
                    status: dupe.status,
                }),
                auto_close: None,
                error: None,
            });
        }

        let position = all
            .iter()
            .filter(|t| t.parent == parent)
            .map(|t| t.position)
            .max()
            .map_or(0, |p| p + 1);
        let session = self.audit_session();
        let closed = !status.is_open();
        let task = Task {
            id: crate::id::new_id(),
            title,
            body: item.body.filter(|b| !b.trim().is_empty()),
            status,
            parent,
            refs,
            wake,
            close,
            position,
            version: version.map(str::to_string),
            closed_version: closed.then(|| version.map(str::to_string)).flatten(),
            created_at: now,
            updated_at: now,
            closed_at: closed.then_some(now),
            session_id: session.clone(),
            touched_session: session,
            origin: Some(self.audit_origin_name()),
            auto: None,
        };
        self.store().upsert_task(&task)?;
        self.audit_task("task_created", &task, None)?;
        Ok(TaskWrite {
            id: Some(task.id),
            created: true,
            matched: None,
            auto_close,
            error: None,
        })
    }

    /// Patch one task. Closing a group by hand closes its open children with
    /// the same status.
    pub fn update_task(&self, id: &str, p: TaskPatch) -> Result<Task> {
        self.require_tasks()?;
        self.settle_tasks()?;
        let before = self
            .store()
            .get_task(id)?
            .ok_or_else(|| Error::NotFound(format!("task {id}")))?;
        let now = crate::store::now();
        let mut t = before.clone();
        if let Some(title) = p.title {
            t.title = clean_title(&title)?;
        }
        if let Some(body) = p.body {
            t.body = body.filter(|b| !b.trim().is_empty());
        }
        if let Some(line) = p
            .progress
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            let stamp = &crate::timespec::iso_instant(now)[..10];
            let mut body = t.body.take().unwrap_or_default();
            if !body.is_empty() && !body.ends_with('\n') {
                body.push('\n');
            }
            body.push_str(&format!("- {stamp}: {line}"));
            t.body = Some(body);
        }
        if let Some(parent) = p.parent {
            let parent = parent
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            if let Some(pid) = &parent {
                self.check_parent(pid, Some(id))?;
            }
            if parent != t.parent {
                let siblings = self.store().all_tasks()?;
                t.position = siblings
                    .iter()
                    .filter(|s| s.parent == parent)
                    .map(|s| s.position)
                    .max()
                    .map_or(0, |x| x + 1);
            }
            t.parent = parent;
        }
        if let Some(refs) = p.refs {
            t.refs = self.check_refs(&refs)?;
        }
        if let Some(wake) = p.wake {
            t.wake = canonical_rule(wake.as_deref(), now)?;
        }
        if let Some(close) = p.close {
            t.close = canonical_rule(close.as_deref(), now)?;
        }
        if let Some(pos) = p.position {
            t.position = pos;
        }
        let mut cascade = None;
        if let Some(status) = p.status
            && status != t.status
        {
            self.apply_status(&mut t, status, now)?;
            if !status.is_open() {
                cascade = Some(status);
            }
        }
        t.updated_at = now;
        t.touched_session = self.audit_session().or(t.touched_session);
        self.store().upsert_task(&t)?;
        self.audit_task("task_updated", &t, Some(&before))?;
        if let Some(status) = cascade {
            for mut child in self
                .store()
                .all_tasks()?
                .into_iter()
                .filter(|c| c.parent.as_deref() == Some(id) && c.status.is_open())
            {
                let child_before = child.clone();
                self.apply_status(&mut child, status, now)?;
                child.auto = Some(format!("{}: closed with its group", status.as_str()));
                child.updated_at = now;
                self.store().upsert_task(&child)?;
                self.audit_task("task_updated", &child, Some(&child_before))?;
            }
        }
        // A child reopening (or closing the last one) moves its group.
        self.settle_tasks()?;
        self.notify_tasks();
        Ok(self.store().get_task(id)?.unwrap_or(t))
    }

    /// Set a manual order: `ids` get positions 0.. in the order given.
    pub fn reorder_tasks(&self, ids: &[String]) -> Result<()> {
        self.require_tasks()?;
        for (i, id) in ids.iter().enumerate() {
            if let Some(mut t) = self.store().get_task(id)?
                && t.position != i as i64
            {
                t.position = i as i64;
                self.store().upsert_task(&t)?;
            }
        }
        self.notify_tasks();
        Ok(())
    }

    /// User-only hard delete (the pane); a group takes its children along.
    pub fn delete_task(&self, id: &str) -> Result<bool> {
        self.require_tasks()?;
        let Some(t) = self.store().get_task(id)? else {
            return Ok(false);
        };
        for child in self
            .store()
            .all_tasks()?
            .into_iter()
            .filter(|c| c.parent.as_deref() == Some(id))
        {
            self.store().delete_task(&child.id)?;
            self.audit_task("task_deleted", &child, Some(&child))?;
        }
        let gone = self.store().delete_task(id)?;
        self.audit_task("task_deleted", &t, Some(&t))?;
        self.notify_tasks();
        Ok(gone)
    }

    /// Fire due rules and settle groups. Runs before every task read and
    /// write, from the brief, and on `set_version`. Returns how many tasks
    /// changed.
    pub fn settle_tasks(&self) -> Result<usize> {
        // Rules don't fire while tasks are off; they settle on the first
        // read after the feature comes back.
        if !self.tasks_enabled() {
            return Ok(0);
        }
        let mut all = self.store().all_tasks()?;
        if all.is_empty() {
            return Ok(0);
        }
        let now = crate::store::now();
        let current = self.current_version()?;
        let mut changed: Vec<(Task, Task)> = Vec::new();

        for t in all.iter_mut() {
            if !t.status.is_open() {
                continue;
            }
            let before = t.clone();
            if let Some(rule) = t.close.as_deref().and_then(|r| Rule::parse(r, now).ok())
                && self.rule_fired(&rule, now, current.as_deref())?
            {
                self.apply_status(t, TaskStatus::Done, now)?;
                t.close = None;
                t.auto = Some(format!("done: {}", rule.describe()));
            } else if matches!(t.status, TaskStatus::Delayed | TaskStatus::Backlog)
                && let Some(rule) = t.wake.as_deref().and_then(|r| Rule::parse(r, now).ok())
                && self.rule_fired(&rule, now, current.as_deref())?
            {
                t.status = TaskStatus::Active;
                t.wake = None;
                t.auto = Some(format!("woke: {}", rule.describe()));
            }
            if *t != before {
                t.updated_at = now;
                changed.push((before, t.clone()));
            }
        }

        // Groups follow their children: every child closed closes the group
        // (done if any was done); an open child reopens a closed group.
        let snapshot = all.clone();
        for g in all.iter_mut() {
            let kids: Vec<&Task> = snapshot
                .iter()
                .filter(|c| c.parent.as_deref() == Some(g.id.as_str()))
                .collect();
            if kids.is_empty() {
                continue;
            }
            let before = g.clone();
            let any_open = kids.iter().any(|c| c.status.is_open());
            if g.status.is_open() && !any_open {
                let status = if kids.iter().any(|c| c.status == TaskStatus::Done) {
                    TaskStatus::Done
                } else {
                    TaskStatus::Dropped
                };
                self.apply_status(g, status, now)?;
                g.auto = Some(format!(
                    "{}: every task in the group closed",
                    status.as_str()
                ));
            } else if !g.status.is_open() && any_open {
                self.apply_status(g, TaskStatus::Active, now)?;
                g.auto = Some("reopened: a task in the group is open again".into());
            }
            if *g != before {
                g.updated_at = now;
                changed.push((before, g.clone()));
            }
        }

        for (before, after) in &changed {
            self.store().upsert_task(after)?;
            self.audit_task("task_auto", after, Some(before))?;
        }
        if !changed.is_empty() {
            self.notify_tasks();
        }
        Ok(changed.len())
    }

    fn rule_fired(&self, rule: &Rule, now: i64, current: Option<&str>) -> Result<bool> {
        Ok(match rule {
            Rule::Date(ts) => now >= *ts,
            Rule::Version(v) => current.is_some_and(|c| {
                version_cmp(c, v).map_or(c == v, |o| o != std::cmp::Ordering::Less)
            }),
            Rule::PastVersion(v) => current.is_some_and(|c| {
                version_cmp(c, v).is_some_and(|o| o == std::cmp::Ordering::Greater)
            }),
            Rule::Node(id) => match self.store().get_node(id)? {
                None => true,
                Some(n) => {
                    n.valid_until.is_some()
                        || n.status
                            .is_some_and(|s| s != crate::types::NodeStatus::Open)
                }
            },
        })
    }

    fn apply_status(&self, t: &mut Task, status: TaskStatus, now: i64) -> Result<()> {
        if status.is_open() {
            t.closed_at = None;
            t.closed_version = None;
        } else if t.status.is_open() {
            t.closed_at = Some(now);
            t.closed_version = self.current_version()?;
        }
        t.status = status;
        Ok(())
    }

    /// `$N` names the N-th item of the current batch; anything else is an id.
    fn resolve_parent(&self, p: &str, earlier: &[TaskWrite]) -> Result<String> {
        if let Some(n) = p.strip_prefix('$') {
            let idx: usize = n.parse().map_err(|_| {
                Error::Config(format!("parent \"{p}\": use $1, $2, … or a task id"))
            })?;
            return earlier
                .get(idx.wrapping_sub(1))
                .and_then(|w| {
                    w.id.clone()
                        .or_else(|| w.matched.as_ref().map(|m| m.id.clone()))
                })
                .ok_or_else(|| {
                    Error::Config(format!(
                        "parent \"{p}\" names no task created earlier in this batch"
                    ))
                });
        }
        Ok(p.to_string())
    }

    /// One level of groups: the parent exists, has no parent itself, and the
    /// task being placed (when it exists) is not a group.
    fn check_parent(&self, parent: &str, child: Option<&str>) -> Result<()> {
        if child == Some(parent) {
            return Err(Error::Config("a task can't be its own group".into()));
        }
        let p = self
            .store()
            .get_task(parent)?
            .ok_or_else(|| Error::NotFound(format!("no task {parent} to use as a group")))?;
        if p.parent.is_some() {
            return Err(Error::Config(format!(
                "task {parent} is already inside a group — groups are one level deep; put this task in {} instead",
                p.parent.unwrap_or_default()
            )));
        }
        if let Some(c) = child
            && self
                .store()
                .all_tasks()?
                .iter()
                .any(|t| t.parent.as_deref() == Some(c))
        {
            return Err(Error::Config(format!(
                "task {c} is a group — groups can't go inside another group"
            )));
        }
        Ok(())
    }

    /// Refs must name existing memory nodes; duplicates fold.
    fn check_refs(&self, refs: &[String]) -> Result<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        let mut missing = Vec::new();
        for r in refs.iter().map(|r| r.trim()).filter(|r| !r.is_empty()) {
            if out.iter().any(|x| x == r) {
                continue;
            }
            if self.store().get_node(r)?.is_none() {
                missing.push(r.to_string());
            }
            out.push(r.to_string());
        }
        if !missing.is_empty() {
            return Err(Error::NotFound(format!(
                "refs name no node in this graph: {}",
                missing.join(", ")
            )));
        }
        Ok(out)
    }

    fn audit_task(&self, action: &str, t: &Task, before: Option<&Task>) -> Result<()> {
        let after = (action != "task_deleted")
            .then(|| serde_json::to_value(t).ok())
            .flatten();
        self.audit_row(
            action,
            "task",
            &t.id,
            Some(t.title.clone()),
            before.and_then(|b| serde_json::to_value(b).ok()),
            after,
        )
    }

    fn notify_tasks(&self) {
        self.emit(ChangeEvent::TasksChanged);
    }

    /// The brief's `## Tasks` lines (empty when there is nothing open or
    /// recently done). Settles rules first.
    pub(crate) fn brief_task_lines(&self) -> Result<Vec<String>> {
        let cfg = self.store().config();
        let tc = &cfg.brief.tasks;
        if !tc.show || !cfg.tasks.enabled {
            return Ok(Vec::new());
        }
        let all = self.tasks()?;
        if all.is_empty() {
            return Ok(Vec::new());
        }
        let current = self.current_version()?;
        let briefs = self.brief_sessions()?;
        let open_top: Vec<&Task> = all
            .iter()
            .filter(|t| t.parent.is_none() && t.status == TaskStatus::Active)
            .collect();
        let mut lines = Vec::new();
        let mut body = Vec::new();
        for t in open_top.iter().take(tc.cap) {
            let v = self.view_of(t, &all, current.as_deref(), &briefs, tc.idle_sessions);
            // `- Title [Task id · marks]`, then for a group its progress and
            // the next step with that step's own id.
            let mut line = format!("- {} [Task {}", t.title, t.id);
            if let Some(c) = &t.close {
                line.push_str(&format!(" · closes {c}"));
            }
            if let Some(from) = &v.carried_from {
                line.push_str(&format!(" · ↻ from {from}"));
            }
            if let Some(n) = v.idle_sessions {
                line.push_str(&format!(" · idle {n} sessions"));
            }
            line.push(']');
            if let Some((done, total)) = v.progress {
                line.push_str(&format!(" — {done}/{total} done"));
                if let Some(next) = all.iter().find(|c| {
                    c.parent.as_deref() == Some(t.id.as_str()) && c.status == TaskStatus::Active
                }) {
                    line.push_str(&format!(", next: {} [Task {}]", next.title, next.id));
                }
            }
            body.push(line);
        }
        if open_top.len() > tc.cap {
            body.push(format!(
                "- …and {} more active — `list_tasks` has them all.",
                open_top.len() - tc.cap
            ));
        }
        let count = |s: TaskStatus| {
            all.iter()
                .filter(|t| t.parent.is_none() && t.status == s)
                .count()
        };
        let (delayed, backlog) = (count(TaskStatus::Delayed), count(TaskStatus::Backlog));
        if delayed + backlog > 0 {
            body.push(format!(
                "Parked: {delayed} delayed, {backlog} backlog — `list_tasks` with status reads them."
            ));
        }
        if tc.recent_done > 0 {
            let done: Vec<String> = all
                .iter()
                .filter(|t| t.status == TaskStatus::Done)
                .take(tc.recent_done)
                .map(|t| match &t.auto {
                    Some(a) => format!("{} ({a})", t.title),
                    None => t.title.clone(),
                })
                .collect();
            if !done.is_empty() {
                body.push(format!("Recently done: {}", done.join("; ")));
            }
        }
        if body.is_empty() {
            return Ok(Vec::new());
        }
        lines.push(
            "\n## Tasks\nYour working list — titles only. `list_tasks` reads bodies; `update_task` moves one along (`progress` appends where you stopped; status done closes it)."
                .to_string(),
        );
        lines.extend(body);
        Ok(lines)
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
