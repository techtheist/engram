//! How the daemon runs ONNX Runtime for its three long-lived sessions
//! (embedder, reranker, NLI). The footprint used to climb and never come back;
//! of four suspects measured, batch width was the whole story ([`batch`]). The
//! BFC arena stays off as cheap insurance, thread pools stay wide
//! ([`intra_threads`]), and the allocator was disproved. None of this changes a
//! model's output (the eval bench proves it). Env knobs: `ENGRAM_ONNX_BATCH`,
//! `ENGRAM_ONNX_THREADS`, `ENGRAM_ONNX_ARENA=1`.

use ort::execution_providers::ExecutionProviderDispatch;

/// Intra-op threads per session; `None` keeps ORT's default (one per core).
/// Capping to four saved ~20 MB and cost ~20% wall-clock, so it is a knob
/// (`ENGRAM_ONNX_THREADS`), not a policy.
pub fn intra_threads() -> Option<usize> {
    std::env::var("ENGRAM_ONNX_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
}

/// Whether the CPU arena allocator is on (off by default; it only grows).
/// `ENGRAM_ONNX_ARENA=1` restores it. Check this before asking ORT to shrink
/// the arena: a shrink with no arena registered is a hard error.
pub fn arena_enabled() -> bool {
    std::env::var("ENGRAM_ONNX_ARENA").is_ok_and(|v| v == "1" || v == "true")
}

/// The execution providers every session uses: CPU with the arena off, so
/// activations are freed when a run ends. `with_memory_pattern` is set beside
/// it wherever we own the builder.
pub fn execution_providers() -> Vec<ExecutionProviderDispatch> {
    vec![
        ort::ep::CPU::default()
            .with_arena_allocator(arena_enabled())
            .build(),
    ]
}

/// Batch ceiling for anything fed to a session: the biggest memory lever and a
/// speed win. The widest batch sizes a session's working set for good, and
/// `BatchLongest` padding makes wide batches pay for padding: 256 → 2 cut
/// wall-clock ~40%. Every eval headline metric is identical; scores agree to
/// ~8 decimals (summation order), so receipts aren't bit-identical.
/// `ENGRAM_ONNX_BATCH` overrides it.
pub fn batch() -> usize {
    std::env::var("ENGRAM_ONNX_BATCH")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(2)
}
