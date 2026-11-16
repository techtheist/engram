//! Task tools: the working list beside the graph.

use crate::*;

#[tool_router(router = tasks_router, vis = "pub(crate)")]
impl Engram {
    #[tool(description = "Add tasks to the working list beside the graph \
        (never searched, never knowledge — the brief lists active titles). \
        One task per piece of work that spans sessions, NOT per step (steps \
        go in the body as a checklist). `tasks`: [{title, body?, status? \
        (active | delayed | backlog; default active), parent? (a task id, or \
        \"$N\" = the N-th item of this call — one level of groups), refs? \
        (memory node ids it is about), wake? / close? (a version \"0.10.0\" \
        or \">0.9.12\", a date \"2026-10-20\" / \"in 3 days\", or \
        \"node:<id>\" = when that note closes)}]. Verdicts: {id, created} | \
        {matched, created: false} = an open task already covers it, \
        update_task that one | {error}. A release-shaped title (\"publish \
        0.9.12\") gets an automatic close rule (`auto_close`).")]
    pub(crate) async fn add_tasks(
        &self,
        Parameters(a): Parameters<AddTasksArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if a.tasks.is_empty() || a.tasks.len() > BATCH_CAP {
            return Err(ErrorData::invalid_params(
                format!("1..={BATCH_CAP} tasks per call"),
                None,
            ));
        }
        let engine = self.write_engine_for(&a.project)?;
        let items = a
            .tasks
            .into_iter()
            .map(|t| engram_core::NewTask {
                title: t.title,
                body: t.body,
                status: t.status,
                parent: t.parent,
                refs: t.refs,
                wake: t.wake,
                close: t.close,
            })
            .collect();
        let results = self.mcp(&engine).add_tasks(items).map_err(task_err)?;
        let results: Vec<serde_json::Value> = results
            .into_iter()
            .map(|w| {
                let mut out = json!(w);
                if let Some(rule) = &w.auto_close {
                    out["note"] = json!(format!(
                        "release-shaped title: this task closes itself once the working version is {rule} — pass close: \"\" on update_task to keep it open"
                    ));
                }
                if w.matched.is_some() {
                    out["note"] = json!(
                        "an open task already covers this — update_task the matched one (progress, body, refs) instead of adding another"
                    );
                }
                out
            })
            .collect();
        self.reply_write(json!({ "results": results }), &a.project)
    }

    #[tool(description = "Change one task: title, body, status (active | \
        delayed | backlog | done | dropped), parent (\"\" = ungroup), refs \
        (replaces the list), wake / close rules (\"\" clears), position, and \
        `progress` — one line appended to the body under today's date (where \
        you stopped, what's next). Closing a group closes its open tasks; \
        a group closes itself when its last task closes.")]
    pub(crate) async fn update_task(
        &self,
        Parameters(a): Parameters<UpdateTaskArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.write_engine_for(&a.project)?;
        let status = a
            .status
            .as_deref()
            .map(engram_core::TaskStatus::parse)
            .transpose()
            .map_err(task_err)?;
        let clearable = |v: Option<String>| v.map(|s| Some(s).filter(|s| !s.trim().is_empty()));
        let patch = engram_core::TaskPatch {
            title: a.title,
            body: clearable(a.body),
            status,
            parent: clearable(a.parent),
            refs: a.refs,
            wake: clearable(a.wake),
            close: clearable(a.close),
            position: a.position,
            progress: a.progress,
        };
        let (task, was_open) = {
            let engine = self.mcp(&engine);
            let was_open = engine
                .get_task(&a.id)
                .map_err(task_err)?
                .is_some_and(|t| t.status.is_open());
            (
                engine.update_task(&a.id, patch).map_err(task_err)?,
                was_open,
            )
        };
        let mut out = json!({ "ok": true, "task": task });
        if was_open && task.status == engram_core::TaskStatus::Done {
            out["note"] = json!(
                "Done. If this work settled something — a decision, a fix, a lesson — capture it now (add_note) and add the new id to this task's refs."
            );
        }
        self.reply_write(out, &a.project)
    }

    #[tool(description = "The working list: tasks with bodies, group \
        progress [closed, total], `carried_from` (opened under an older \
        version) and `idle_sessions`. Default = open tasks (active, delayed, \
        backlog); `status` takes one or more comma-separated statuses, \
        \"closed\" or \"all\". `parent` = one group's tasks; `ref` = tasks \
        about one node.")]
    pub(crate) async fn list_tasks(
        &self,
        Parameters(a): Parameters<ListTasksArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        use engram_core::TaskStatus as S;
        let mut statuses = Vec::new();
        for word in a.status.as_deref().unwrap_or("").split(',') {
            match word.trim() {
                "" | "open" => {}
                "closed" => statuses.extend([S::Done, S::Dropped]),
                "all" => statuses.extend([S::Active, S::Delayed, S::Backlog, S::Done, S::Dropped]),
                w => statuses.push(S::parse(w).map_err(task_err)?),
            }
        }
        if a.status
            .as_deref()
            .is_some_and(|s| s.split(',').any(|w| w.trim() == "open"))
        {
            statuses.extend([S::Active, S::Delayed, S::Backlog]);
        }
        let engine = self.engine_for(&a.project)?;
        let tasks = self
            .mcp(&engine)
            .list_tasks(&engram_core::TaskFilter {
                statuses,
                parent: a.parent,
                ref_id: a.ref_id,
            })
            .map_err(task_err)?;
        self.reply(&json!({ "total": tasks.len(), "tasks": tasks }))
    }
}
