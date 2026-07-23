//! The sqlite→tepin cutover as a library operation (PLAN §7D): shared by
//! `engram-alpha migrate` and the 0.7.* auto-migration at open. JSON
//! export/import is the vehicle for nodes + edges (embeddings regenerated on
//! the way in), suspects and the audit journal ride over verbatim, counts are
//! verified, and the SQLite source is never modified — it stays behind as the
//! backup. The new store is built at a `.tepin.part` sibling and published to
//! `graph.tepin` only once complete, so a crashed or raced migration can
//! never leave a half-written file that `resolve_db_path` would prefer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::engine::{AuditOrigin, Engine};
use crate::rag::Embedder;
use crate::store_sqlite::SqliteStore;
use crate::store_tepin::TepinStore;
use crate::{Error, Result};

pub struct MigrationSummary {
    pub nodes: usize,
    pub edges: usize,
    pub suspects: usize,
    pub audit: usize,
    /// The published `graph.tepin` path.
    pub dst: PathBuf,
}

pub fn migrate_to_tepin(
    src_path: &Path,
    embedder: Arc<dyn Embedder>,
    origin: AuditOrigin,
) -> Result<MigrationSummary> {
    let dst_path = src_path.with_extension("tepin");
    if dst_path.exists() {
        return Err(Error::Io(format!(
            "{} already exists — remove it first to rebuild from the SQLite source",
            dst_path.display()
        )));
    }
    let part = src_path.with_extension("tepin.part");
    // A crashed earlier attempt leaves a .part behind; it's disposable.
    let _ = std::fs::remove_file(&part);

    let src = Engine::new(SqliteStore::open(src_path)?, Box::new(embedder.clone()));
    let graph = src.export()?;
    let suspects = src.store().all_suspects()?;
    let audit_total = src.store().audit_page(None, None, 1)?.total.max(0) as usize;
    let audit = src.store().audit_page(None, None, audit_total)?;

    let built = (|| -> Result<MigrationSummary> {
        let mut dst = Engine::new(TepinStore::open(&part)?, Box::new(embedder.clone()));
        dst.set_audit_origin(origin);
        // Journal first (oldest-first so seq keeps chronological order), then
        // the graph — import appends its own "imported" row, which lands
        // last, as the migration's own mark in the history.
        for entry in audit.entries.iter().rev() {
            dst.store().add_audit(entry)?;
        }
        for s in &suspects {
            dst.store().upsert_suspect(s)?;
        }
        let summary = dst.import(graph)?;
        dst.store()
            .set_embed_version(src.store().embed_version()?)?;
        if !embedder.is_fake() {
            dst.store().set_embed_model(&dst.embed_model_id())?;
        }

        let src_stats = src.store().stats()?;
        let dst_stats = dst.store().stats()?;
        if src_stats.nodes != dst_stats.nodes || src_stats.edges != dst_stats.edges {
            return Err(Error::Io(format!(
                "count mismatch after migration (nodes {} -> {}, edges {} -> {}) — {} is untouched",
                src_stats.nodes,
                dst_stats.nodes,
                src_stats.edges,
                dst_stats.edges,
                src_path.display()
            )));
        }
        let dst_suspects = dst.store().all_suspects()?.len();
        if suspects.len() != dst_suspects {
            return Err(Error::Io(format!(
                "suspect queue mismatch after migration ({} -> {dst_suspects})",
                suspects.len()
            )));
        }
        Ok(MigrationSummary {
            nodes: summary.nodes,
            edges: summary.edges,
            suspects: suspects.len(),
            audit: audit_total,
            dst: dst_path.clone(),
        })
    })();

    match built {
        Ok(summary) => {
            // hard_link fails if the target exists — the no-replace publish.
            // Losing that race means another process finished an equivalent
            // migration first; theirs is as good as ours.
            let published = std::fs::hard_link(&part, &dst_path);
            let _ = std::fs::remove_file(&part);
            match published {
                Ok(()) => Ok(summary),
                Err(_) if dst_path.exists() => Ok(summary),
                Err(e) => Err(Error::Io(format!("publishing {}: {e}", dst_path.display()))),
            }
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}
