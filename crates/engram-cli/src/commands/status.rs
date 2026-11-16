//! `status`: the machine core, its clients and each project's setup.

use crate::*;

/// `engram-alpha status` (issue #5): a thin client of the core's GET /system
/// and GET /projects — core identity, model residency, registered projects
/// with their lock state, and every connected client, in humane terms.
pub(crate) fn run_status(args: StatusArgs) -> anyhow::Result<()> {
    if args.setup {
        return run_status_setup(args.json);
    }
    let Some(port) = machine_core() else {
        if args.json {
            println!("{}", serde_json::json!({ "running": false }));
        } else {
            println!("engram core: not running");
            println!(
                "  start it with `engram-alpha serve` (from a repo) or let an MCP client spawn it"
            );
        }
        return Ok(());
    };
    let system = doctor::http_get(port, "/system")
        .and_then(|b| serde_json::from_str::<serde_json::Value>(&b).ok())
        .context("the core answered /health but not /system")?;
    let mut projects = doctor::http_get(port, "/projects")
        .and_then(|b| serde_json::from_str::<serde_json::Value>(&b).ok())
        .unwrap_or_else(|| serde_json::json!([]));
    if let Some(list) = projects.as_array_mut() {
        for p in list {
            let setup = p["root"]
                .as_str()
                .and_then(|r| upkeep::inspect(Path::new(r)))
                .map(|i| i.to_json());
            if let Some(setup) = setup {
                p["setup"] = setup;
            }
        }
    }

    if args.json {
        println!(
            "{}",
            serde_json::json!({
                "running": true,
                "port": port,
                "system": system,
                "projects": projects,
            })
        );
        return Ok(());
    }

    let now = engram_core::now();
    let core = &system["processes"]["core"];
    let uptime = system["daemon"]["uptime_secs"].as_i64().unwrap_or(0);
    println!("engram core");
    println!(
        "  pid {} · v{} · up {} · port {port}",
        core["pid"].as_u64().unwrap_or(0),
        core["version"]
            .as_str()
            .or(system["version"].as_str())
            .unwrap_or("?"),
        humane_duration(uptime),
    );
    if let Some(home) = core["home"].as_str() {
        println!("  home {home}");
    }
    let models = &system["models_state"];
    match (models["state"].as_str(), models["since"].as_i64()) {
        (Some("unloaded_idle"), Some(since)) => println!(
            "  models: unloaded (idle since {})",
            humane_ago(now.saturating_sub(since))
        ),
        (Some(state), Some(since)) => println!(
            "  models: {state} (since {})",
            humane_ago(now.saturating_sub(since))
        ),
        _ => println!(
            "  models: {}",
            if system["model_cached"].as_bool() == Some(true) {
                "cached"
            } else {
                "not downloaded"
            }
        ),
    }

    println!("\nprojects");
    match projects.as_array().filter(|p| !p.is_empty()) {
        Some(list) => {
            for p in list {
                let name = p["name"].as_str().unwrap_or("?");
                let place = p["root"].as_str().or(p["db"].as_str()).unwrap_or("?");
                let state = if p["open"].as_bool() == Some(true) {
                    "store open (held by the core)"
                } else {
                    "registered"
                };
                println!("  {name} · {place} · {state}");
                // The home graph has no repo; a project's setup (issue #19).
                if let Some(i) = p["root"]
                    .as_str()
                    .and_then(|r| upkeep::inspect(Path::new(r)))
                {
                    println!("    {}", i.describe());
                }
            }
        }
        None => println!("  (none registered)"),
    }

    println!("\nclients");
    match system["processes"]["clients"]
        .as_array()
        .filter(|c| !c.is_empty())
    {
        Some(clients) => {
            for c in clients {
                // "mcp-bridge (mcp-go)" — which client bound where, and via
                // what. Older bridges never sent a name; the bare kind stays.
                let kind = match c["client"].as_str() {
                    Some(name) => format!("{} ({name})", c["kind"].as_str().unwrap_or("?")),
                    None => c["kind"].as_str().unwrap_or("?").to_string(),
                };
                println!(
                    "  {kind} · pid {} · {} · connected {} · seen {}",
                    c["pid"].as_u64().unwrap_or(0),
                    c["root"].as_str().unwrap_or("?"),
                    humane_ago(now.saturating_sub(c["connected_at"].as_i64().unwrap_or(now))),
                    humane_ago(now.saturating_sub(c["last_seen"].as_i64().unwrap_or(now))),
                );
            }
        }
        None => println!("  (none connected)"),
    }
    Ok(())
}

/// `engram-alpha status --setup` (issue #19): every registered project's
/// setup as this binary judges it — also what `update` prints, asked of the
/// new binary after a swap. Only projects with something due are listed in
/// text; `--json` lists all.
pub(crate) fn run_status_setup(json: bool) -> anyhow::Result<()> {
    let projects = upkeep::inspect_registered();
    if json {
        let list: Vec<serde_json::Value> = projects
            .iter()
            .map(|(p, i)| {
                serde_json::json!({
                    "name": p.name,
                    "root": p.root,
                    "setup": i.as_ref().map(upkeep::Inspection::to_json),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "version": env!("CARGO_PKG_VERSION"), "projects": list })
        );
        return Ok(());
    }
    let due: Vec<_> = projects
        .iter()
        .filter_map(|(p, i)| i.as_ref().filter(|i| !i.is_current()).map(|i| (p, i)))
        .collect();
    let known = projects.iter().filter(|(_, i)| i.is_some()).count();
    if due.is_empty() {
        println!(
            "project setup: {} current for v{}",
            match known {
                0 => "no projects registered,".to_string(),
                1 => "1 project".into(),
                n => format!("all {n} projects"),
            },
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    println!(
        "project setup against v{} ({} of {known} with something due):",
        env!("CARGO_PKG_VERSION"),
        due.len()
    );
    for (p, i) in due {
        println!("  {} ({}): {}", p.name, p.root, i.describe());
    }
    Ok(())
}

/// `7381` → `"2h 3m"` — two units at most, seconds only under a minute.
pub(crate) fn humane_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (d, h, m, s) = (
        secs / 86_400,
        (secs % 86_400) / 3_600,
        (secs % 3_600) / 60,
        secs % 60,
    );
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, m) => format!("{m}m {s}s"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

pub(crate) fn humane_ago(secs: i64) -> String {
    if secs <= 0 {
        "just now".into()
    } else {
        format!("{} ago", humane_duration(secs))
    }
}
