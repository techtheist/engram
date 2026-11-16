//! The project registry and the folder picker.

use crate::*;

/// Every project the hub can reach: current, home, registry — the pane's
/// switcher and the Settings registry view both read this.
pub(crate) async fn list_projects(State(state): State<Arc<AppState>>) -> Json<Vec<ProjectInfo>> {
    Json(state.hub.projects())
}

#[derive(Deserialize)]
pub(crate) struct RegisterBody {
    /// Absolute path of the repo to register (its `.engram/graph.db` is
    /// created lazily on first access).
    pub(crate) path: String,
}

pub(crate) async fn register_project(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterBody>,
) -> Result<Json<registry::ProjectEntry>, AppError> {
    let root = std::path::PathBuf::from(&body.path);
    if !root.is_dir() {
        return Err(AppError::Core(Error::Project(format!(
            "not a directory: {}",
            body.path
        ))));
    }
    let db = root.join(".engram/graph.db");
    let entry = registry::register(&root, &db)?;
    // The daemon this pane talks to serves the list — refresh is one GET away.
    let _ = state;
    Ok(Json(entry))
}

/// Withdraw a project from the registry — awareness only; its data stays
/// where it lives. The current project and the home graph are not entries.
pub(crate) async fn unregister_project(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    if id == registry::HOME_PROJECT
        || id == state.hub.current().id
        || id == state.hub.current().name
    {
        return Err(AppError::Core(Error::Project(
            "the current project and the home graph are not registry entries".into(),
        )));
    }
    let removed = registry::unregister(&id)?;
    if !removed {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub(crate) struct FsDirsParams {
    /// Absolute directory to list; omitted = the user's home directory.
    pub(crate) path: Option<String>,
}

/// The pane's folder picker (a browser can't reveal absolute paths): child
/// directories, dot-dirs hidden, unreadable ones skipped, each flagged when it
/// holds an `.engram` graph or a git repo.
pub(crate) async fn fs_dirs(
    Query(p): Query<FsDirsParams>,
) -> Result<Json<serde_json::Value>, AppError> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(std::path::PathBuf::from);
    let start = p
        .path
        .map(std::path::PathBuf::from)
        .or_else(|| home.clone())
        .ok_or_else(|| AppError::Core(Error::Project("no home directory".into())))?;
    let listing = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, Error> {
        let path = start
            .canonicalize()
            .map_err(|e| Error::Project(format!("not a readable directory: {e}")))?;
        if !path.is_dir() {
            return Err(Error::Project(format!(
                "not a directory: {}",
                path.display()
            )));
        }
        let mut dirs = Vec::new();
        for entry in std::fs::read_dir(&path)
            .map_err(|e| Error::Project(format!("can't list {}: {e}", path.display())))?
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || !entry.path().is_dir() {
                continue;
            }
            let p = entry.path();
            dirs.push(json!({
                "name": name,
                "path": p.display().to_string(),
                "engram": p.join(".engram/graph.db").is_file(),
                "git": p.join(".git").exists(),
            }));
        }
        dirs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Ok(json!({
            "path": path.display().to_string(),
            "parent": path.parent().map(|p| p.display().to_string()),
            "home": home.map(|h| h.display().to_string()),
            "dirs": dirs,
        }))
    })
    .await
    .expect("directory listing never panics — every entry error is a skip")?;
    Ok(Json(listing))
}

// ---- node handlers ------------------------------------------------------
