//! Embedding composition upgrades and the digest scan.

use super::*;

#[test]
fn embed_composition_upgrade_reembeds_only_with_real_vectors() {
    let db = std::env::temp_dir().join(format!("engram-compose-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);

    // FakeEmbedder math with the real-embedder contract — exercises the
    // upgrade path without ONNX.
    struct NotFake(FakeEmbedder);
    impl Embedder for NotFake {
        fn dim(&self) -> usize {
            self.0.dim()
        }
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.0.embed(texts)
        }
    }

    // Seed a node, then rewind the version marker to legacy.
    let e = Engine::new(
        SqliteStore::open(&db).unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    let mut n = new_node(NodeType::Decision, "composed", "");
    n.code_refs = vec!["crates/engram-core/src/rag.rs".into()];
    e.add_node(n).unwrap();
    e.store().set_embed_version(0).unwrap();

    // A fake embedder must refuse to touch an existing graph's vectors.
    assert_eq!(e.ensure_embed_composition().unwrap(), 0);
    assert_eq!(e.store().embed_version().unwrap(), 0);
    drop(e);

    // A real one upgrades every node and stamps the version.
    let e = Engine::new(
        SqliteStore::open(&db).unwrap(),
        Box::new(NotFake(FakeEmbedder::default())),
    );
    assert_eq!(e.ensure_embed_composition().unwrap(), 1);
    assert_eq!(e.store().embed_version().unwrap(), EMBED_COMPOSITION);
    drop(e);
    let _ = std::fs::remove_file(&db);
}

// ---- digestion tier 1: offline marker scan --------------------

/// A fresh scan root per test — a shared fixed dir would leak files between
/// runs and break the count assertions.
pub(super) fn digest_root(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("engram-digest-{label}-{}", id::new_id()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn digest_scan_maps_markers_to_types() {
    let root = digest_root("markers");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "fn main() {}\n// TODO(alice): wire the retry loop\n// FIXME broken on empty input */\n",
    )
    .unwrap();

    let scan = digest::scan(&root);
    assert_eq!(scan.candidates.len(), 2);
    assert!(!scan.truncated);

    let todo = &scan.candidates[0];
    assert_eq!(todo.marker, "TODO");
    assert_eq!(todo.suggested_type, NodeType::Intent);
    assert_eq!(todo.text, "wire the retry loop");
    assert_eq!(todo.file, "src/main.rs");
    assert_eq!(todo.line, 2);

    let fixme = &scan.candidates[1];
    assert_eq!(fixme.marker, "FIXME");
    assert_eq!(fixme.suggested_type, NodeType::Problem);
    // Comment closer stripped, text extracted without a colon separator.
    assert_eq!(fixme.text, "broken on empty input");
    assert_eq!(fixme.line, 3);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn digest_scan_respects_gitignore_and_trash_dirs() {
    let root = digest_root("ignore");
    std::fs::write(root.join(".gitignore"), "vendor/\n").unwrap();
    std::fs::create_dir_all(root.join("vendor")).unwrap();
    std::fs::create_dir_all(root.join("logs")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("vendor/dep.js"), "// TODO ignored dependency\n").unwrap();
    std::fs::write(root.join("logs/app.log"), "TODO in a log line\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "// TODO: the real one\n").unwrap();

    let scan = digest::scan(&root);
    assert_eq!(scan.candidates.len(), 1);
    assert_eq!(scan.candidates[0].file, "src/lib.rs");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn digest_scan_skips_binary_huge_and_prose_todos() {
    let root = digest_root("robust");
    // NUL bytes = binary; counted as skipped, never an error.
    std::fs::write(root.join("blob.bin"), b"TODO\x00binary").unwrap();
    // Oversized files are generated/vendored, not hand-written markers.
    std::fs::write(root.join("big.js"), "x".repeat(1_000_001)).unwrap();
    // Lowercase "todo" in prose is not a work marker.
    std::fs::write(root.join("notes.md"), "my todo list\nmastodon api\n").unwrap();

    let scan = digest::scan(&root);
    assert!(scan.candidates.is_empty());
    assert_eq!(scan.files_skipped, 2);
    assert_eq!(scan.files_scanned, 1);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn digest_scan_redacts_marker_text() {
    let root = digest_root("redact");
    std::fs::write(
        root.join("cfg.rs"),
        "// TODO rotate password=hunter2-very-secret-value soon\n",
    )
    .unwrap();

    let scan = digest::scan(&root);
    assert_eq!(scan.candidates.len(), 1);
    let text = &scan.candidates[0].text;
    assert!(text.contains("[REDACTED]"), "got: {text}");
    assert!(!text.contains("hunter2"));

    let _ = std::fs::remove_dir_all(&root);
}
