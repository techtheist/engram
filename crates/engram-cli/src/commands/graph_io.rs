//! `brief`, `export` and `import`.

use crate::*;

/// Resolve the owning daemon's route for one of this store's endpoints:
/// `{suffix}` on a repo-local daemon, `/projects/{id}{suffix}` on the machine
/// core (registering the repo on the way). `None` = no daemon owns the store
/// and the caller may open the file directly.
pub(crate) fn daemon_route(db: &Path, suffix: &str) -> Option<(u16, String)> {
    let url = resolve_mcp_target(db)?.replace("/mcp", suffix);
    let rest = url.strip_prefix("http://127.0.0.1:")?;
    let (port, path) = rest.split_once('/')?;
    Some((port.parse().ok()?, format!("/{path}")))
}

pub(crate) fn run_brief(args: BriefArgs) -> anyhow::Result<()> {
    // Thin client first: a running daemon owns the store (and on TepinDB is
    // the only process allowed to). A repo-launched daemon serves this
    // project at /brief; the machine core serves it at the scoped route.
    let db = engram_core::resolve_db_path(&args.db);
    // No explicit budget → the owning graph's configured one (daemon-side
    // when bridged, config read when opening directly).
    let suffix = match args.max_chars {
        Some(n) => format!("/brief?max_chars={n}"),
        None => "/brief".to_string(),
    };
    if let Some((port, path)) = daemon_route(&db, &suffix)
        && let Some(text) = doctor::http_get(port, &path)
    {
        println!("{}", text.trim_end());
        return Ok(());
    }
    // The hub form appends the home-graph section; a brief is a
    // read, so it deliberately does not touch the registry.
    let (hub, _) = build_hub(&db, args.fake_embeddings, false)?;
    let max_chars = hub.current_engine().locked().brief_chars(args.max_chars);
    println!("{}", hub.brief(max_chars)?);
    Ok(())
}

pub(crate) fn run_export(args: ExportArgs) -> anyhow::Result<()> {
    // Thin client first: a daemon owning this store serves
    // GET /export — a direct open would trip the tepin single-owner lock.
    let db = engram_core::resolve_db_path(&args.db);
    let snapshot: ExportGraph = if let Some((port, path)) = daemon_route(&db, "/export")
        && let Some(body) = doctor::http_get(port, &path)
    {
        serde_json::from_str(&body).context("parsing the daemon's export")?
    } else {
        build_engine(&db, args.fake_embeddings, false)?.export()?
    };
    let json = serde_json::to_string_pretty(&snapshot)?;
    match args.out {
        Some(path) => {
            std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
            tracing::info!(
                "exported {} nodes / {} edges to {}",
                snapshot.nodes.len(),
                snapshot.edges.len(),
                path.display()
            );
        }
        None => println!("{json}"),
    }
    Ok(())
}

pub(crate) fn run_import(args: ImportArgs) -> anyhow::Result<()> {
    let data = std::fs::read_to_string(&args.file)
        .with_context(|| format!("reading {}", args.file.display()))?;
    // Parse locally first — a malformed file fails here, not at the daemon.
    let graph: ExportGraph = serde_json::from_str(&data).context("parsing export JSON")?;
    let db = engram_core::resolve_db_path(&args.db);
    // Thin client first: the owning daemon runs the import (re-embedding
    // takes a while — generous timeout), which also feeds the pane's SSE.
    let (nodes, edges) = if let Some((port, path)) = daemon_route(&db, "/import") {
        let resp =
            doctor::http_post_timeout(port, &path, &data, std::time::Duration::from_secs(600))
                .context("the daemon rejected the import — check its log")?;
        let v: serde_json::Value =
            serde_json::from_str(&resp).context("parsing the daemon's import summary")?;
        (
            v["nodes"].as_u64().unwrap_or_default() as usize,
            v["edges"].as_u64().unwrap_or_default() as usize,
        )
    } else {
        let mut engine = build_engine(&db, args.fake_embeddings, false)?;
        engine.set_audit_origin(engram_core::AuditOrigin::cli());
        let summary = engine.import(graph)?;
        (summary.nodes, summary.edges)
    };
    tracing::info!(
        "imported {nodes} nodes / {edges} edges into {}",
        db.display()
    );
    Ok(())
}
