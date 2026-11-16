//! Daemon discovery files and the .gitignore entry.

use crate::*;

/// `~/.engram/daemon.json` — the machine core's advertisement, next to the
/// registry. Stale files are harmless: every reader health-checks the port.
pub(crate) fn write_machine_daemon_file(port: u16, db_display: &str) {
    let Some(home) = registry::engram_home() else {
        return;
    };
    let _ = std::fs::create_dir_all(&home);
    let body = serde_json::json!({
        "port": port,
        "url": format!("http://127.0.0.1:{port}"),
        "pid": std::process::id(),
        "db": db_display,
        "version": engram_core::advertised_version(),
    });
    if let Err(e) = std::fs::write(home.join("daemon.json"), format!("{body:#}\n")) {
        tracing::warn!("couldn't write the machine daemon file: {e}");
    }
}

pub(crate) fn remove_machine_daemon_file() {
    if let Some(home) = registry::engram_home() {
        let _ = std::fs::remove_file(home.join("daemon.json"));
    }
}

/// `.engram/daemon.json`: how plugins and the skill find the pane port (they
/// regex-scrape `port`, so keep the shape). Readers health-check the port, so a
/// stale file is harmless.
pub(crate) fn write_daemon_file(db: &Path, port: u16, pid: u32) {
    let Some(dir) = db.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    let db_display = std::fs::canonicalize(db)
        .unwrap_or_else(|_| db.to_path_buf())
        .display()
        .to_string();
    let body = serde_json::json!({
        "port": port,
        "url": format!("http://127.0.0.1:{port}"),
        "pid": pid,
        "db": db_display,
        "version": engram_core::advertised_version(),
    });
    if let Err(e) = std::fs::write(dir.join("daemon.json"), format!("{body:#}\n")) {
        tracing::warn!("couldn't write daemon.json: {e}");
    }
}

/// Best-effort: make sure the repo's `.gitignore` excludes the graph DB dir
/// (it's personal/local). Idempotent; never fatal; a dir git already ignores
/// in any way (`/.engram/`, a global rule — issue #12) is left alone.
pub(crate) fn ensure_gitignored(db: &Path) {
    let Some(entry) = std::env::current_dir()
        .ok()
        .and_then(|cwd| gitignore::db_dir_entry(db, &cwd))
    else {
        return;
    };
    if gitignore::is_ignored(Path::new("."), &entry) {
        return;
    }
    let gitignore = Path::new(".gitignore");
    let current = std::fs::read_to_string(gitignore).unwrap_or_default();
    let mut next = current;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(&entry);
    next.push('\n');
    if std::fs::write(gitignore, next).is_ok() {
        tracing::info!("added {entry} to .gitignore");
    }
}
