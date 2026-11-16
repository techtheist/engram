//! Tasks.

use crate::*;

#[derive(Deserialize)]
pub(crate) struct TaskListParams {
    /// Comma-separated statuses, "open", "closed" or "all" (default all —
    /// the pane's board shows every column).
    pub(crate) status: Option<String>,
    pub(crate) parent: Option<String>,
    #[serde(rename = "ref")]
    pub(crate) ref_id: Option<String>,
}

pub(crate) async fn list_tasks(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Query(p): Query<TaskListParams>,
) -> Result<Json<Vec<engram_core::TaskView>>, AppError> {
    use engram_core::TaskStatus as S;
    let mut statuses = Vec::new();
    for word in p.status.as_deref().unwrap_or("all").split(',') {
        match word.trim() {
            "" | "all" => statuses.extend([S::Active, S::Delayed, S::Backlog, S::Done, S::Dropped]),
            "open" => statuses.extend([S::Active, S::Delayed, S::Backlog]),
            "closed" => statuses.extend([S::Done, S::Dropped]),
            w => statuses.push(S::parse(w)?),
        }
    }
    let engine = state.engine_arc(&scope)?;
    let tasks = pane(&engine).list_tasks(&engram_core::TaskFilter {
        statuses,
        parent: p.parent,
        ref_id: p.ref_id,
    })?;
    Ok(Json(tasks))
}

pub(crate) async fn create_task(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(input): Json<engram_core::NewTask>,
) -> Result<Json<engram_core::TaskWrite>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let mut out = pane(&engine).add_tasks(vec![input])?;
    let w = out.pop().ok_or(AppError::NotFound)?;
    if let Some(e) = &w.error {
        return Err(AppError::Core(Error::Config(e.clone())));
    }
    Ok(Json(w))
}

pub(crate) async fn get_task(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<Json<engram_core::Task>, AppError> {
    let engine = state.engine_arc(&scope)?;
    let task = pane(&engine).get_task(&id)?;
    task.map(Json).ok_or(AppError::NotFound)
}

/// A pane patch: absent = unchanged; "" on body/parent/wake/close clears.
#[derive(Deserialize)]
pub(crate) struct TaskPatchBody {
    pub(crate) title: Option<String>,
    pub(crate) body: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) parent: Option<String>,
    pub(crate) refs: Option<Vec<String>>,
    pub(crate) wake: Option<String>,
    pub(crate) close: Option<String>,
    pub(crate) position: Option<i64>,
    pub(crate) progress: Option<String>,
}

pub(crate) async fn patch_task(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
    Json(p): Json<TaskPatchBody>,
) -> Result<Json<engram_core::Task>, AppError> {
    let clearable = |v: Option<String>| v.map(|s| Some(s).filter(|s| !s.trim().is_empty()));
    let patch = engram_core::TaskPatch {
        title: p.title,
        body: clearable(p.body),
        status: p
            .status
            .as_deref()
            .map(engram_core::TaskStatus::parse)
            .transpose()?,
        parent: clearable(p.parent),
        refs: p.refs,
        wake: clearable(p.wake),
        close: clearable(p.close),
        position: p.position,
        progress: p.progress,
    };
    let engine = state.engine_arc(&scope)?;
    let task = pane(&engine).update_task(&id, patch)?;
    Ok(Json(task))
}

pub(crate) async fn delete_task(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let engine = state.engine_arc(&scope)?;
    if pane(&engine).delete_task(&id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

#[derive(Deserialize)]
pub(crate) struct TaskOrder {
    pub(crate) ids: Vec<String>,
}

pub(crate) async fn order_tasks(
    State(state): State<Arc<AppState>>,
    scope: Scope,
    Json(p): Json<TaskOrder>,
) -> Result<StatusCode, AppError> {
    let engine = state.engine_arc(&scope)?;
    pane(&engine).reorder_tasks(&p.ids)?;
    Ok(StatusCode::NO_CONTENT)
}
