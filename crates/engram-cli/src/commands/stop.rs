//! `stop`: end the machine core, and the process helpers it needs.

use crate::*;

/// Ask the core to shut down over `POST /shutdown`, wait for the process to
/// exit, and fall back to a PID kill when it doesn't answer. Pre-0.8.8 per-repo
/// daemons get the old health-verified kill.
pub(crate) fn run_stop() -> anyhow::Result<()> {
    let mut stopped = 0;
    if let Some(home) = registry::engram_home() {
        stopped += stop_machine_core(&home.join("daemon.json"), false);
    }

    // Legacy per-repo daemons, discovered from the daemon files we already
    // keep (never by process-name matching), health-verified before each
    // kill so a stale file can't take down an unrelated reused pid. Files
    // that advertised the now-stopped core clean themselves up here too.
    for entry in registry::load().projects {
        let Some(dir) = Path::new(&entry.db).parent() else {
            continue;
        };
        let file = dir.join("daemon.json");
        let Ok(raw) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) else {
            let _ = std::fs::remove_file(&file);
            continue;
        };
        let (Some(port), Some(pid)) = (meta["port"].as_u64(), meta["pid"].as_u64()) else {
            continue;
        };
        if doctor::http_get(port as u16, "/health").is_none() {
            // Nothing lives there — the file is stale; clean it up.
            let _ = std::fs::remove_file(&file);
            continue;
        }
        let ok = terminate_pid(pid as u32);
        if ok {
            // Give it a moment, then confirm the port went quiet.
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(250));
                if doctor::http_get(port as u16, "/health").is_none() {
                    break;
                }
            }
            let _ = std::fs::remove_file(&file);
            println!("stopped {} (pid {pid}, port {port})", entry.name);
            stopped += 1;
        } else {
            eprintln!(
                "couldn't stop {} (pid {pid}) — stop it manually",
                entry.name
            );
        }
    }
    if stopped == 0 {
        println!("nothing to stop — no healthy engram daemons advertised");
    }
    Ok(())
}

/// Stop the machine core advertised in `file`. Returns how many processes
/// were stopped (0 or 1). `quiet` suppresses the stdout report — the version
/// handshake runs this from inside an MCP bridge, whose stdout IS the
/// JSON-RPC channel (progress goes to tracing/stderr there).
pub(crate) fn stop_machine_core(file: &Path, quiet: bool) -> usize {
    let Ok(raw) = std::fs::read_to_string(file) else {
        return 0;
    };
    let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) else {
        let _ = std::fs::remove_file(file);
        return 0;
    };
    let Some(port) = meta["port"].as_u64().map(|p| p as u16) else {
        let _ = std::fs::remove_file(file);
        return 0;
    };
    if doctor::http_get(port, "/health").is_none() {
        let _ = std::fs::remove_file(file);
        return 0;
    }
    // What is about to be cut loose — reported after the stop.
    let clients = doctor::http_get(port, "/system")
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .and_then(|v| v["processes"]["clients"].as_array().map(std::vec::Vec::len));
    let pid = meta["pid"].as_u64().map(|p| p as u32);

    // Judge "gone" on the process, not /health: a health-silent core can still
    // hold its locks (issue #8).
    let mut gone = false;
    if doctor::http_post(port, "/shutdown", "{}").is_some() {
        gone = wait_core_exit(pid, port, 60); // 15s: the exit path drains engines first
    }
    if !gone
        && let Some(pid) = pid
        && terminate_pid(pid)
    {
        gone = wait_core_exit(Some(pid), port, 20);
    }
    if !gone && let Some(pid) = pid {
        // Last resort — both backends are crash-safe (fsync-per-commit / WAL),
        // and a core that ignored /shutdown AND SIGTERM is wedged, not working.
        eprintln!("the core (pid {pid}) didn't exit in time — force-killing it");
        if force_kill_pid(pid) {
            gone = wait_core_exit(Some(pid), port, 20);
        }
    }
    if !gone {
        eprintln!(
            "couldn't stop the machine core (pid {}, port {port}) — stop it manually",
            pid.unwrap_or_default()
        );
        return 0;
    }
    let _ = std::fs::remove_file(file); // the core removes it itself; belt and braces
    if !port_is_free(port) {
        eprintln!(
            "warning: port {port} is still in use after the stop — another process is listening there"
        );
    }
    if !quiet {
        match clients {
            Some(n) => println!(
                "stopped machine core (pid {}, port {port}) — {n} connected client(s) released",
                pid.unwrap_or_default()
            ),
            None => println!(
                "stopped machine core (pid {}, port {port})",
                pid.unwrap_or_default()
            ),
        }
    }
    1
}

/// Wait for the core to actually exit: poll the pid when we have one (the
/// only signal that means the store locks are released), fall back to the
/// health probe for pre-pid daemon files. 4 ticks per second.
pub(crate) fn wait_core_exit(pid: Option<u32>, port: u16, ticks: u32) -> bool {
    for _ in 0..ticks {
        std::thread::sleep(std::time::Duration::from_millis(250));
        let gone = match pid {
            Some(p) => !pid_alive(p),
            None => doctor::http_get(port, "/health").is_none(),
        };
        if gone {
            return true;
        }
    }
    false
}

/// Is the process alive? Safe against pid recycling here: the pid comes from
/// a daemon file whose port answered /health moments ago — the core itself
/// wrote it and was alive to prove it.
pub(crate) fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

pub(crate) fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

pub(crate) fn force_kill_pid(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        // terminate_pid already uses taskkill /F — force IS terminate there.
        terminate_pid(pid)
    }
}

pub(crate) fn terminate_pid(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .arg(pid.to_string())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}
