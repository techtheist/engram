//! The task list's engine surface: writes, reads, and rule settling.

use super::*;

impl Engine {
    /// Whether this graph keeps a task list (config `tasks.enabled`).
    pub fn tasks_enabled(&self) -> bool {
        self.store().config().tasks.enabled
    }

    /// Refuse a task call on a graph with tasks off.
    pub(super) fn require_tasks(&self) -> Result<()> {
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
    pub(super) fn view_of(
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
    pub(super) fn brief_sessions(&self) -> Result<Vec<(i64, String)>> {
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

    pub(super) fn add_one_task(
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

        for t in &mut all {
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
        for g in &mut all {
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

    pub(super) fn rule_fired(&self, rule: &Rule, now: i64, current: Option<&str>) -> Result<bool> {
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

    pub(super) fn apply_status(&self, t: &mut Task, status: TaskStatus, now: i64) -> Result<()> {
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
    pub(super) fn resolve_parent(&self, p: &str, earlier: &[TaskWrite]) -> Result<String> {
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
    pub(super) fn check_parent(&self, parent: &str, child: Option<&str>) -> Result<()> {
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
    pub(super) fn check_refs(&self, refs: &[String]) -> Result<Vec<String>> {
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

    pub(super) fn audit_task(&self, action: &str, t: &Task, before: Option<&Task>) -> Result<()> {
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

    pub(super) fn notify_tasks(&self) {
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
