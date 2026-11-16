//! Opening engines and hubs.

use crate::*;

/// Open one store as an engine wired to the shared models. This is the hub's
/// engine factory: every project store the daemon serves goes through here.
pub(crate) fn open_engine(db: &Path, models: &Models) -> engram_core::Result<Engine> {
    if let Some(dir) = db.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| engram_core::Error::Io(format!("creating {}: {e}", dir.display())))?;
    }
    let set = models.current();
    // An unmigrated graph.db migrates to tepin at open (SQLite stays as backup).
    // Needs a real embedder, since migration regenerates vectors; failure falls
    // back to SQLite.
    let resolved = engram_core::resolve_db_path(db);
    if resolved.extension().is_some_and(|e| e == "db")
        && resolved.exists()
        && !set.embedder.is_fake()
    {
        match engram_core::migrate_to_tepin(
            &resolved,
            set.embedder.clone(),
            engram_core::AuditOrigin::daemon(),
        ) {
            Ok(s) => tracing::info!(
                "migrated {} nodes / {} edges to {} — {} is untouched (your backup)",
                s.nodes,
                s.edges,
                s.dst.display(),
                resolved.display()
            ),
            Err(e) => tracing::warn!("auto-migration to TepinDB failed, staying on SQLite: {e}"),
        }
    }
    let store = engram_core::open_store(db)?;
    let mut engine = Engine::with_store(store, Box::new(set.embedder.clone()));
    // Write-time code_ref checks resolve against the repo the DB lives in
    // (<root>/.engram/graph.db), falling back to the launch cwd.
    let root = db
        .canonicalize()
        .ok()
        .and_then(|p| {
            let dir = p.parent()?;
            (dir.file_name()? == ".engram")
                .then(|| dir.parent().map(std::path::Path::to_path_buf))?
        })
        .or_else(|| std::env::current_dir().ok());
    if let Some(root) = root {
        engine.set_repo_root(root);
    }
    // The desired at-rest encryption comes from machine settings; each store's
    // meta records the actual state and the reconcile converges them.
    let machine = engram_core::settings::load();
    engine.set_desired_encryption(Some(machine.encrypt_graph), Some(machine.encrypt_history));
    // History layer: the sibling store beside the curated one, opened
    // by the engine when the graph's history config is enabled (opt-in).
    engine.set_history_path(engram_core::history::history_store_path(&resolved));
    if let Some(r) = &set.reranker {
        engine.set_reranker(Box::new(r.clone()));
    }
    if let Some(n) = &set.nli {
        engine.set_nli(Box::new(n.clone()));
    }
    // Guarded vector upgrades, model identity first (a swapped embedding
    // model rebuilds vector storage and re-embeds — PLAN §7A model
    // selection), then the composition catch-up. Never fatal at startup.
    match engine.ensure_embed_model() {
        Ok(0) => {}
        Ok(n) => tracing::info!(
            "re-embedded {n} nodes for the {} embedding model",
            engine.embed_model_id().name
        ),
        Err(e) => tracing::warn!("embedding-model upgrade failed: {e}"),
    }
    match engine.ensure_embed_composition() {
        Ok(0) => {}
        Ok(n) => tracing::info!("re-embedded {n} nodes for the current embedding composition"),
        Err(e) => tracing::warn!("embedding-composition upgrade failed: {e}"),
    }
    match engine.ensure_field_index() {
        Ok(0) => {}
        Ok(n) => tracing::info!("reindexed {n} nodes for the graph's indexed custom fields"),
        Err(e) => tracing::warn!("custom-field index upgrade failed: {e}"),
    }
    match engine.ensure_encryption() {
        Ok(0) => {}
        Ok(n) => tracing::info!("encryption reconcile reprocessed {n} rows"),
        Err(e) => tracing::warn!("encryption reconcile failed: {e}"),
    }
    Ok(engine)
}

/// Single-engine form, kept for export/import.
pub(crate) fn build_engine(
    db: &Path,
    fake_embeddings: bool,
    cortex: bool,
) -> anyhow::Result<Engine> {
    let models = load_models(fake_embeddings, cortex)?;
    open_engine(db, &models).with_context(|| format!("opening {}", db.display()))
}

/// A short-lived multi-project hub over one store — the brief's
/// direct-open fallback when no daemon owns the store. Long-lived hubs are
/// the machine core's job ([`build_core_hub`]); nothing here registers or
/// keeps locks past the command.
pub(crate) fn build_hub(
    db: &Path,
    fake_embeddings: bool,
    cortex: bool,
) -> anyhow::Result<(Arc<Hub>, Models)> {
    let db = &engram_core::resolve_db_path(db);
    let models = load_models(fake_embeddings, cortex)?;
    let engine = open_engine(db, &models).with_context(|| format!("opening {}", db.display()))?;
    let factory_models = models.clone();
    let factory: engram_core::EngineFactory = Box::new(move |db| open_engine(db, &factory_models));
    let hub = Arc::new(Hub::new(Arc::new(Mutex::new(engine)), None, Some(factory)));
    Ok((hub, models))
}

/// The core hub launched outside any project (v0.6.2 serve-anywhere): the
/// home graph is the current project; registered repos open lazily.
pub(crate) fn build_core_hub(fake_embeddings: bool) -> anyhow::Result<(Arc<Hub>, Models)> {
    let home = registry::home_db_path().context("no home directory for the home graph")?;
    let models = load_models(fake_embeddings, true)?;
    let engine =
        open_engine(&home, &models).with_context(|| format!("opening {}", home.display()))?;
    let factory_models = models.clone();
    let factory: engram_core::EngineFactory = Box::new(move |db| open_engine(db, &factory_models));
    Ok((
        Arc::new(Hub::new_home(Arc::new(Mutex::new(engine)), Some(factory))),
        models,
    ))
}
