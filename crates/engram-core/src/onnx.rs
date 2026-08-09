//! How the daemon runs ONNX Runtime — one place, three sessions.
//!
//! Engram holds an embedder, a reranker and an NLI session open for the whole
//! process lifetime. On stock settings the daemon's footprint climbed steadily
//! through a normal workload and never came back down, ending far above what
//! the weights account for.
//!
//! Four suspects were measured, and only one of them was real:
//!
//! * **Batch width — the whole story.** fastembed defaults to 256, and the
//!   working set that batch demands is what the process ends up holding.
//!   Narrowing it removed nearly all of the growth *and* made inference
//!   substantially faster, because `BatchLongest` padding means a wide batch
//!   spends most of its compute on padding. See [`batch`].
//! * **The BFC arena** — the obvious culprit, and a red herring. Disabling it
//!   changed neither footprint nor wall-clock once the batch was narrow.
//!   It stays off as cheap insurance (it is the mechanism that makes a peak
//!   permanent) but it is not what was costing the memory.
//! * **Thread pools** — three sessions × one thread per core. Capping them
//!   moved the footprint by ~20 MB and cost ~20% of wall-clock, so the
//!   default stays wide. See [`intra_threads`].
//! * **The allocator holding freed pages** — disproved outright. The heap is
//!   genuinely live, not cached: `malloc_zone_pressure_relief` on a timer
//!   reclaimed nothing at all, and vmmap showed the in-use byte count barely
//!   moving while the footprint climbed.
//!
//! Everything here is an *allocation and scheduling* policy. None of it changes
//! a single number any of the three models produces — that is the whole point,
//! and the eval bench is what proves it.
//!
//! Tunable at runtime without a rebuild, for A/B runs and for users who want a
//! different trade: `ENGRAM_ONNX_BATCH`, `ENGRAM_ONNX_THREADS`,
//! `ENGRAM_ONNX_ARENA=1`.

use ort::execution_providers::ExecutionProviderDispatch;

/// Intra-op threads per session. `None` leaves ORT's default (one per core).
///
/// Capping this looks like it should save memory — three sessions on a
/// ten-core machine is thirty inference threads, each with its own scratch.
/// Measured, it does not: with the batch ceiling below in place, dropping to
/// four threads moved the daemon's footprint by ~20 MB and cost ~20% of
/// wall-clock on the eval bench. So the default stays wide and this is a
/// knob, not a policy — `ENGRAM_ONNX_THREADS` is there for people who would
/// rather give up retrieval latency than have a background daemon saturate
/// their cores.
pub fn intra_threads() -> Option<usize> {
    std::env::var("ENGRAM_ONNX_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
}

/// Whether the CPU arena allocator stays on. Off by default: for this
/// workload the arena is a memory leak with a throughput benefit we can't
/// use. `ENGRAM_ONNX_ARENA=1` restores it for comparison runs.
///
/// Callers that own their `run` must consult this before asking ORT to shrink
/// the arena afterwards — requesting a shrink with no arena registered is a
/// hard error ("Did not find an arena based allocator registered for
/// device-id combination in the memory arena shrink list"), not a no-op.
pub fn arena_enabled() -> bool {
    std::env::var("ENGRAM_ONNX_ARENA").is_ok_and(|v| v == "1" || v == "true")
}

/// The execution providers every Engram session is built with.
///
/// `ep::CPU` with the arena disabled is the entire trick: activations go
/// through plain malloc and are returned when the run ends, instead of being
/// parked in an arena that only ever grows. `SessionBuilder::with_memory_pattern`
/// is the sibling knob and is set alongside this wherever we own the builder.
pub fn execution_providers() -> Vec<ExecutionProviderDispatch> {
    vec![
        ort::ep::CPU::default()
            .with_arena_allocator(arena_enabled())
            .build(),
    ]
}

/// Batch ceiling for anything we feed a session.
///
/// This is the single biggest memory lever in the daemon, and — against
/// intuition — it is also a throughput win. Two effects, both measured:
///
/// * **Memory.** Whatever the widest batch turns out to be is what the
///   session's working set is sized for, and that peak is never given back.
///   Measured end to end on this repo's graph, the daemon's growth over a
///   search + `check_claim` + conflict-scan workload scales almost linearly
///   with this number — a wide batch grows it by an order of magnitude more
///   than a narrow one.
/// * **Speed.** Padding is `BatchLongest`, so a wide batch pads every short
///   text out to the longest one in it and pays full attention cost on the
///   padding. Engram's texts vary a lot in length, so wide batches burn most
///   of their compute on padding. On the eval bench, dropping from
///   fastembed's default of 256 to 2 cut wall-clock by ~40% and CPU time by
///   more than half — reproducibly, across repeated runs.
///
/// Correctness is not a trade here: every row in these batches is scored
/// independently of its neighbours, and padding is masked out. Measured on
/// the eval bench at sizes 100 and 500, old width against new: **every
/// headline metric identical on all seven arms** — recall, focus, noise,
/// oblique recall, false-positive rate, tokens per query. What does move is
/// float summation order (a different batch shape blocks the kernels
/// differently), which shows up as mean scores agreeing to about eight
/// decimal places rather than exactly. Once, downstream of that, a brief
/// came out two tokens shorter. So: numerically equivalent, not
/// bit-identical — worth knowing before treating a receipt diff as a
/// regression.
///
/// `ENGRAM_ONNX_BATCH` overrides it.
pub fn batch() -> usize {
    std::env::var("ENGRAM_ONNX_BATCH")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(2)
}
