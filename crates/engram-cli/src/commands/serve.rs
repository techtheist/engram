//! `serve`: ensure the core and register the project.

use crate::*;

/// What a `serve` invocation should become — decided by looking at the
/// machine, not by flags (the one-smart-binary Principle, v0.6.2).
pub(crate) enum ServeRole {
    /// Serve with this store as the current project (repo dir, or explicit --db).
    Project(PathBuf),
    /// No project here (~, non-git dir, or a declined init): run the machine
    /// core with the home graph as the current project.
    CoreOnly,
}

pub(crate) fn serve_role(args: &ServeArgs) -> anyhow::Result<ServeRole> {
    let resolved = engram_core::resolve_db_path(&args.db);
    if args.db != Path::new(".engram/graph.db") {
        // An explicit --db is an explicit answer.
        return Ok(ServeRole::Project(resolved));
    }
    let cwd = std::env::current_dir()?;
    if resolved.exists() || cwd.join(".engram").exists() {
        return Ok(ServeRole::Project(resolved));
    }
    if cwd.join(".git").exists() {
        // A git repo without a graph: offer to init — never create silently.
        if propose_init(&cwd) {
            return Ok(ServeRole::Project(resolved));
        }
        eprintln!(
            "Skipped — you can initialize this repo later with `engram-alpha setup`, another `serve`, or from the pane."
        );
    }
    Ok(ServeRole::CoreOnly)
}

/// Interactive init consent: only on a real terminal, default yes.
pub(crate) fn propose_init(repo: &Path) -> bool {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return false;
    }
    eprint!(
        "No Engram graph in {} yet — initialize one here? [Y/n] ",
        repo.display()
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    matches!(line.trim().to_lowercase().as_str(), "" | "y" | "yes")
}

/// The light launcher: make sure the machine core is
/// running, hand it this repo's project, refresh the repo-local
/// advertisement, print the pane URL, and exit — the core keeps serving.
pub(crate) async fn serve(args: ServeArgs) -> anyhow::Result<()> {
    if args.http_only {
        eprintln!(
            "note: --http-only is deprecated — serve now ensures the shared machine core and \
             exits; with this flag it stays up while the core is healthy (supervisor back-compat)."
        );
    }
    // Version handshake: converging on an older core would leave every
    // session on stale code (and stale tool descriptions) after an update.
    if retire_older_core() {
        eprintln!(
            "stopped an older engram core — this binary (v{}) starts a fresh one",
            engram_core::advertised_version()
        );
    }
    let port = match machine_core() {
        Some(port) => port,
        None => {
            eprintln!("starting the engram core…");
            spawn_core_and_wait(args.http_port, args.fake_embeddings)?
        }
    };
    match serve_role(&args)? {
        ServeRole::Project(db) => {
            ensure_gitignored(&db);
            // The store may not exist yet (the core creates it on first
            // open), so absolutize by hand — a canonicalize would fail and
            // leave a relative path in the registry.
            let db_abs = if db.is_absolute() {
                db.clone()
            } else {
                std::env::current_dir()?.join(&db)
            };
            // Registration is the whole handoff: the registry is the shared
            // awareness file, and the core opens registered stores lazily on
            // first access (pane switch, bridge connect).
            match repo_root_of(&db_abs).or_else(|| std::env::current_dir().ok()) {
                Some(root) => match registry::register(&root, &db_abs) {
                    Ok(entry) => {
                        tracing::info!("registered project '{}' with the core", entry.name);
                        // The repo-local advertisement plugins scrape the pane
                        // port from — pointing at the core.
                        write_daemon_file(&db_abs, port, machine_core_pid().unwrap_or_default());
                        // Eager first open (issue #8): one loopback brief makes the core create the
                        // store now, since SessionStart hooks gate on the file existing.
                        let probe = format!(
                            "/brief?project={}&max_chars=1",
                            percent_encode(&root.display().to_string())
                        );
                        let opened = doctor::http_get_timeout(
                            port,
                            &probe,
                            std::time::Duration::from_secs(20),
                        )
                        .is_some();
                        let store = engram_core::resolve_db_path(&db_abs);
                        if opened && store.exists() {
                            println!("store ready: {}", store.display());
                        } else {
                            eprintln!(
                                "warning: the core didn't open the store at {} — the first \
                                 assistant session will create it on demand",
                                store.display()
                            );
                        }
                    }
                    Err(e) => tracing::warn!("couldn't register this repo: {e}"),
                },
                None => tracing::warn!("couldn't resolve this repo's root — not registered"),
            }
            // Installed skills and instruction blocks follow the binary
            //: untouched copies update in place, edited ones stay,
            // and copies git tracks are held for `setup --refresh`.
            if let Some(root) = repo_root_of(&db_abs) {
                let found = upkeep::run(&root);
                for line in skillsync::report(&root, &found) {
                    println!("{line}");
                }
            }
            // First-run nudge: confirm what is wired, and mention installed assistants
            // that aren't. Scans the full roster, since detection misses off-PATH CLIs.
            if let Ok(cwd) = std::env::current_dir() {
                let wired: Vec<&str> = setup::AGENTS
                    .iter()
                    .copied()
                    .filter(|a| setup::is_wired(&cwd, a))
                    .collect();
                if !wired.is_empty() {
                    println!("engram wired for: {}", wired.join(", "));
                }
                let unwired: Vec<&str> = setup::detect_agents()
                    .into_iter()
                    .filter(|a| !wired.contains(a))
                    .collect();
                if !unwired.is_empty() {
                    // The installer deliberately never wires anything (it is
                    // often run from $HOME) — this nudge, with the exact
                    // command for exactly what was found, is the onboarding
                    // path instead.
                    eprintln!(
                        "found {} — installed but not connected to Engram in this repo; wire {}: engram-alpha setup --cli {}",
                        unwired.join(", "),
                        if unwired.len() == 1 { "it" } else { "them" },
                        unwired.join(",")
                    );
                }
            }
        }
        ServeRole::CoreOnly => {
            tracing::info!("no project here — the core serves the home graph");
        }
    }
    println!("Engram core running — pane: http://127.0.0.1:{port}");
    if args.http_only {
        // Back-compat blocking mode: a process supervisor watching this
        // invocation effectively watches the core's health through it —
        // exiting immediately would restart-loop it.
        block_while_core_healthy(port).await;
        eprintln!("the engram core on port {port} is gone — exiting");
    }
    Ok(())
}

/// Percent-encode a query value for the hand-rolled loopback GET. `/` stays
/// literal — the values are absolute paths and the daemon reads them raw.
pub(crate) fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Poll the core's /health and return when it stops answering (one retry to
/// ride out a transient hiccup).
pub(crate) async fn block_while_core_healthy(port: u16) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        if doctor::http_get(port, "/health").is_none() {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if doctor::http_get(port, "/health").is_none() {
                return;
            }
        }
    }
}
