//! The corpus generator.
//!
//! Every fact is about an **invented** subject ("Kelnor lease broker"), so no
//! model can answer it from pretraining and no agent can answer it from having
//! read this repo. The answer arrives through retrieval or it does not arrive.
//! That is the whole point: it makes the measurement immune to the bias that
//! sinks every dogfood benchmark.
//!
//! Every vocabulary carries two columns — the wording the fact uses, and a
//! word-disjoint paraphrase used only by oblique questions. That is what lets
//! an oblique question describe a fact without sharing its vocabulary, which
//! is the only way to tell semantic retrieval apart from keyword matching.
//!
//! Slots are enumerated by mixed radix rather than drawn randomly, so each
//! fact's (component, descriptor, qualifier) triple is unique by construction
//! up to `MAX_PER_KIND`. The seed permutes the vocabularies, so runs differ
//! without ever colliding.

use std::collections::HashSet;

use serde::Serialize;

use engram_core::{NodeStatus, NodeType};

use crate::profile::Profile;
use crate::rng::Rng;

/// Every s1 pool (PARAMS, FAILURES, FORBIDDEN, SYMPTOMS, BEHAVIOURS) holds
/// exactly this many entries, and every s2 pool (CONSTRAINTS, TRIGGERS,
/// REASONS, CONDITIONS, CAUSES) likewise — the slot arithmetic in
/// `Vocab::slots` indexes them interchangeably per kind.
const S1_LEN: usize = 25;
const S2_LEN: usize = 25;

/// Unique (component, descriptor, qualifier) triples per kind. Slot space now
/// exceeds the invented-name space, so the binding cap on corpus size is the
/// 4096 names (minus one control subject per four tested facts), not this.
pub const MAX_PER_KIND: usize = COMPONENTS.len() * S1_LEN * S2_LEN;

const ONSET: [&str; 16] = [
    "kel", "van", "tor", "mel", "dru", "zan", "fir", "lor", "bre", "quen", "sil", "har", "nov",
    "yss", "garn", "ulm",
];
// 16 × 16 × 16 = 4096 distinct invented names. Slot space outgrew this when
// the pools widened, so the name supply (facts plus one control subject per
// four tested facts) is what actually caps corpus size, at ~3200 facts.
const MID: [&str; 16] = [
    "a", "o", "i", "en", "ar", "ul", "ei", "yn", "ae", "ou", "ir", "el", "or", "um", "ai", "yr",
];
const CODA: [&str; 16] = [
    "nor", "vis", "thi", "dex", "lun", "mar", "tek", "sen", "vor", "qui", "dal", "rin", "bek",
    "tos", "fen", "wyn",
];

/// Mostly software infrastructure, deliberately salted with laboratory and
/// abstract-process categories so retrieval is never tuned to one genre's
/// vocabulary. A component may be named by an oblique question — it is the
/// category, not the fact — so its words must stay out of every paraphrase
/// column.
const COMPONENTS: [&str; 16] = [
    "ingest job",
    "lease broker",
    "shard router",
    "replay buffer",
    "token vault",
    "edge cache",
    "sweep worker",
    "index writer",
    "assay station",
    "specimen locker",
    "calibration bench",
    "telemetry mast",
    "provenance ledger",
    "quorum relay",
    "escrow chamber",
    "entropy siphon",
];

/// (parameter, unit, paraphrase)
const PARAMS: [(&str, &str, &str); S1_LEN] = [
    ("retry budget", "attempts", "how often it tries again"),
    ("lease window", "seconds", "how long a grant stays valid"),
    ("flush interval", "milliseconds", "how often it writes out"),
    ("fan-out limit", "branches", "how wide it spreads work"),
    (
        "backoff ceiling",
        "seconds",
        "the longest it waits between tries",
    ),
    ("batch size", "records", "how much it groups per pass"),
    (
        "probe timeout",
        "milliseconds",
        "how long a health check may hang",
    ),
    (
        "spill threshold",
        "megabytes",
        "when it starts writing to disk",
    ),
    (
        "decay half-life",
        "hours",
        "how quickly stored strength fades",
    ),
    (
        "sampling stride",
        "readings",
        "the spacing between successive measurements",
    ),
    (
        "confidence floor",
        "percent",
        "the certainty below which nothing counts",
    ),
    (
        "quarantine window",
        "days",
        "how long suspects stay isolated",
    ),
    ("grace period", "seconds", "how long overruns are forgiven"),
    (
        "checkpoint cadence",
        "minutes",
        "how often restart state is saved",
    ),
    (
        "dedupe horizon",
        "entries",
        "how far back repeats are noticed",
    ),
    (
        "warmup allowance",
        "requests",
        "how much slack a cold spin-up gets",
    ),
    (
        "eviction ratio",
        "percent",
        "what share gets pushed out each pass",
    ),
    (
        "gossip interval",
        "milliseconds",
        "how often peers exchange rumours",
    ),
    (
        "snapshot depth",
        "generations",
        "how many past copies stay kept",
    ),
    (
        "drain deadline",
        "seconds",
        "how long a shutdown may linger",
    ),
    (
        "parity stripe",
        "blocks",
        "the breadth of the redundancy striping",
    ),
    (
        "rewind cursor lag",
        "records",
        "how far behind the reread may trail",
    ),
    (
        "compaction floor",
        "segments",
        "the level beneath which nothing gets squeezed together",
    ),
    ("quota refresh", "hours", "how often the cap tops back up"),
    (
        "stall tolerance",
        "ticks",
        "how long zero progress is acceptable",
    ),
];

/// Every pair below is (wording the fact uses, word-disjoint paraphrase).
const CONSTRAINTS: [(&str, &str); S2_LEN] = [
    (
        "the upstream limiter resets on a fixed window",
        "the throttle above it starts over each period",
    ),
    (
        "the coordinator drops idle sessions",
        "inactive connections get reaped centrally",
    ),
    (
        "the replica set votes once per epoch",
        "followers only elect at term boundaries",
    ),
    (
        "the vault rotates its keys on a schedule",
        "secrets are cycled on a timer",
    ),
    (
        "the archive seals one segment per interval",
        "storage closes a chunk at a steady cadence",
    ),
    (
        "the fence tokens all expire together",
        "barrier grants lapse simultaneously",
    ),
    (
        "the scheduler admits a single drain at a time",
        "only one flush is allowed through at once",
    ),
    (
        "the manifest is rewritten wholesale",
        "the index file is replaced in full",
    ),
    (
        "the detector saturates past its rated exposure",
        "the sensor maxes out beyond its design load",
    ),
    (
        "the centrifuge must spin down before reloading",
        "the rotor has to coast to rest ahead of a refill",
    ),
    (
        "the ledger admits one writer per epoch",
        "a single scribe holds the pen each term",
    ),
    (
        "the observatory publishes on a fixed almanac",
        "release dates follow a preset calendar",
    ),
    (
        "the gateway meters admissions per tenant",
        "entry is rationed for each customer",
    ),
    (
        "the journal truncates only at a checkpoint",
        "history is trimmed just where restart points exist",
    ),
    (
        "the balancer rehomes work only between epochs",
        "tasks move to new owners at term boundaries alone",
    ),
    (
        "the freezer defrosts on a rotating schedule",
        "thawing happens in a fixed rotation",
    ),
    (
        "the notary countersigns in daily batches",
        "official approval lands once per day in bulk",
    ),
    (
        "the mixer requires a settled queue before dosing",
        "ingredients only feed in once the backlog is calm",
    ),
    (
        "the antenna realigns during quiet hours",
        "pointing is corrected when nothing is transmitting",
    ),
    (
        "the warehouse audits one aisle per shift",
        "stock checks cover a single row at a time",
    ),
    (
        "the compiler pins its toolchain per release",
        "build tools are frozen for every version cut",
    ),
    (
        "the incubator holds temperature within one degree",
        "warmth is kept inside a tight band",
    ),
    (
        "the turnstile admits badge holders only",
        "entry needs a registered pass",
    ),
    (
        "the scheduler forbids overlapping maintenance",
        "upkeep windows can never coincide",
    ),
    (
        "the printer queues plates strictly first-come",
        "output order matches arrival, no exceptions",
    ),
];

const FAILURES: [(&str, &str); S1_LEN] = [
    ("drops writes", "loses acknowledged data"),
    ("double-counts rows", "reports inflated totals"),
    ("loses its cursor", "forgets how far it had read"),
    ("stalls the queue", "wedges everything behind it"),
    ("corrupts its checkpoint", "leaves unreadable restart state"),
    ("re-emits duplicates", "sends the same payload twice"),
    ("truncates the tail", "discards the newest entries"),
    ("skips the fsync", "acknowledges before durability"),
    (
        "mislabels its vials",
        "puts the wrong name on each container",
    ),
    (
        "inverts its polarity",
        "flips positive and negative readings",
    ),
    ("overruns its dwell", "lingers longer than scheduled"),
    ("garbles its telemetry", "scrambles what it reports home"),
    (
        "starves its consumers",
        "leaves downstream readers with nothing",
    ),
    ("shreds its manifest", "destroys its own table of contents"),
    (
        "forgets its offsets",
        "loses track of positions previously read",
    ),
    (
        "poisons its snapshot",
        "writes a restore image that cannot be trusted",
    ),
    (
        "swallows its errors",
        "reports success where something went wrong",
    ),
    (
        "fragments its heap",
        "splinters memory into unusable pieces",
    ),
    (
        "misroutes its callbacks",
        "sends completions to the wrong caller",
    ),
    (
        "expires its own grants",
        "cancels permissions it just issued",
    ),
    ("clones its timers", "fires the same alarm repeatedly"),
    ("desyncs its mirrors", "lets copies drift out of agreement"),
    (
        "hoards its scratch pages",
        "never gives borrowed space back",
    ),
    (
        "blackholes its retries",
        "repeat attempts vanish leaving no trace",
    ),
    (
        "upends its ordering",
        "delivers things in scrambled sequence",
    ),
];

const TRIGGERS: [(&str, &str); S2_LEN] = [
    (
        "the lease expires mid-flush",
        "a grant lapses while data is still moving",
    ),
    ("the shard rebalances", "ownership moves between partitions"),
    ("a replica lags behind", "a follower falls out of step"),
    ("the clock steps backwards", "time jumps in reverse"),
    (
        "the ring buffer wraps",
        "the circular store overtakes itself",
    ),
    (
        "a retry lands out of order",
        "a repeat arrives before the original",
    ),
    (
        "the segment seals early",
        "a chunk closes ahead of schedule",
    ),
    (
        "two drains overlap",
        "a second flush starts before the first ends",
    ),
    (
        "the coolant loop cavitates",
        "bubbles form in the cooling circuit",
    ),
    (
        "the reagent expires overnight",
        "the chemical goes flat between shifts",
    ),
    (
        "the almanac rolls over",
        "the published calendar starts a new page",
    ),
    (
        "a stray magnet passes the bay",
        "unshielded metal drifts too close",
    ),
    (
        "the daylight saving shift lands",
        "the wall time jumps an hour by decree",
    ),
    (
        "a tenant exceeds its envelope",
        "one customer outgrows its slice",
    ),
    ("the failover rehearsal runs", "the standby drill kicks off"),
    (
        "a checksum disagrees on arrival",
        "verification fails as the data comes in",
    ),
    (
        "the backfill overtakes live traffic",
        "historical catch-up outruns the fresh stream",
    ),
    (
        "a poison message recirculates",
        "one bad payload keeps coming back",
    ),
    (
        "the license check phones home",
        "the entitlement ping goes out",
    ),
    (
        "a bulk delete lands mid-compaction",
        "a mass removal arrives while squeezing is underway",
    ),
    ("the mains power browns out", "supply voltage sags briefly"),
    (
        "a leap second is injected",
        "an extra tick is wedged into the day",
    ),
    (
        "the standby promotes itself",
        "the backup takes over on its own",
    ),
    (
        "a schema migration half-applies",
        "a structure change stops partway",
    ),
    (
        "the dedupe window slides",
        "the repeat-spotting span moves along",
    ),
];

const FORBIDDEN: [(&str, &str); S1_LEN] = [
    (
        "share a cursor between shards",
        "one read position serving two partitions",
    ),
    (
        "write before the fence lands",
        "publishing ahead of the barrier",
    ),
    (
        "trust the replica clock",
        "relying on a follower's sense of time",
    ),
    (
        "retry inside the lock",
        "repeating work while still holding exclusion",
    ),
    ("cache the lease token", "keeping a grant past its issue"),
    (
        "widen the fence without a drain",
        "growing the barrier while work is in flight",
    ),
    (
        "ack before the segment seals",
        "confirming receipt ahead of the seal",
    ),
    (
        "reuse a spill file",
        "recycling scratch storage between runs",
    ),
    (
        "recalibrate against a live specimen",
        "adjusting the instrument on an active sample",
    ),
    ("splice two lineages", "merging unrelated ancestry lines"),
    ("skip the blank control", "omitting the empty reference run"),
    (
        "hand-tune the weights mid-flight",
        "nudging parameters while the system is airborne",
    ),
    ("mutate a sealed segment", "editing a chunk after its seal"),
    (
        "promote an unverified backup",
        "elevating a restore nobody tested",
    ),
    (
        "bypass the admission meter",
        "slipping past the intake gate",
    ),
    (
        "rewind a shared cursor",
        "moving a common read position backwards",
    ),
    (
        "fork the settlement record",
        "splitting the book of account in two",
    ),
    (
        "suppress a failing healthcheck",
        "silencing a probe that says no",
    ),
    (
        "interleave two restore streams",
        "braiding distinct recoveries together",
    ),
    (
        "outrun the replication horizon",
        "writing faster than copies can keep up",
    ),
    ("reuse a burned nonce", "presenting a one-time secret twice"),
    (
        "widen the voting circle mid-ballot",
        "growing the electorate while votes are cast",
    ),
    (
        "archive an open incident",
        "shelving a problem that is live",
    ),
    (
        "trust a client-supplied timestamp",
        "believing the sender's own clock",
    ),
    (
        "hot-patch the verifier itself",
        "changing the checker while it checks",
    ),
];

const REASONS: [(&str, &str); S2_LEN] = [
    (
        "the invariant it protects cannot be re-derived after the fact",
        "nothing downstream can reconstruct the guarantee later",
    ),
    (
        "recovery has no way to tell a stale value from a fresh one",
        "restart cannot distinguish old state from new",
    ),
    (
        "the failure is silent and only shows up as drift much later",
        "the damage surfaces long after its cause",
    ),
    (
        "there is no ordering left to reconstruct once it is violated",
        "sequence information is gone for good",
    ),
    (
        "the audit trail becomes unusable for the whole epoch",
        "the record of what happened stops being trustworthy",
    ),
    (
        "downstream consumers assume it and cannot defend themselves",
        "everything after it is written expecting the guarantee",
    ),
    (
        "it turns a recoverable fault into a permanent one",
        "a fixable error becomes unfixable",
    ),
    (
        "the repair costs more than the operation ever saved",
        "cleanup outweighs any gain",
    ),
    (
        "no replication can ever confirm a contaminated baseline",
        "a tainted origin defeats every rerun",
    ),
    (
        "the chain of custody breaks and cannot be respliced",
        "provenance once severed stays severed",
    ),
    (
        "consent covers the original purpose only",
        "permission does not stretch to new uses",
    ),
    (
        "the instrument's warranty voids on first opening",
        "cracking the case forfeits all support",
    ),
    (
        "the blast radius includes every tenant at once",
        "one mistake reaches all customers together",
    ),
    (
        "the rollback path was never built",
        "there is no route back by design",
    ),
    (
        "insurance cover lapses the moment it happens",
        "the policy stops paying right then",
    ),
    (
        "the alarm that would catch it is the thing disabled",
        "the very siren meant to fire is off",
    ),
    (
        "no operator can re-derive the missing constant",
        "the lost figure cannot be recomputed by hand",
    ),
    (
        "the certificate chain cannot be reissued quickly",
        "new credentials take too long to mint",
    ),
    (
        "silent corruption spreads through every mirror",
        "bad bytes copy themselves everywhere",
    ),
    (
        "the legal hold forbids touching those records",
        "compliance freezes that data outright",
    ),
    (
        "the downstream contract promises exactly-once",
        "consumers were guaranteed no repeats",
    ),
    (
        "only the failing node knew the session keys",
        "the secrets died with the machine that held them",
    ),
    (
        "the deadline is enforced by physics, not policy",
        "no configuration can buy the time back",
    ),
    (
        "every later measurement inherits the skew",
        "all subsequent readings carry the tilt",
    ),
    (
        "the customer-visible number can never be reused",
        "an identifier the public saw is spent forever",
    ),
];

const SYMPTOMS: [(&str, &str); S1_LEN] = [
    ("stalls", "stops making progress"),
    ("leaks handles", "never returns its file descriptors"),
    ("reorders acks", "confirms out of sequence"),
    ("spins on backoff", "burns cpu waiting"),
    (
        "drifts its watermark",
        "loses track of its own progress marker",
    ),
    ("thrashes the cache", "evicts what it is about to need"),
    ("duplicates its fence", "issues the same barrier twice"),
    ("starves the drain", "never lets the flush finish"),
    ("saturates its detector", "pegs the sensor at maximum"),
    ("skews its baseline", "tilts every measurement it takes"),
    ("orphans its batches", "abandons work it had accepted"),
    (
        "echoes its own commands",
        "repeats instructions back to itself",
    ),
    ("flaps its liveness", "keeps toggling between up and down"),
    (
        "inflates its estimates",
        "predicts far more than materialises",
    ),
    (
        "clips its bursts",
        "cuts short every spike it should absorb",
    ),
    ("misplaces its permits", "cannot say who holds which grant"),
    ("staggers its heartbeats", "checks in at ragged intervals"),
    ("shadows its primary", "silently repeats the leader's work"),
    (
        "overfills its journal",
        "writes more log than the space allows",
    ),
    ("stutters its stream", "delivers output in fits and bursts"),
    (
        "ignores its backpressure",
        "keeps pushing when told to slow",
    ),
    ("splits its brain", "two halves each think they lead"),
    ("lags its dashboard", "feeds the operator stale numbers"),
    ("churns its connections", "opens and drops links nonstop"),
    ("misfiles its alerts", "routes warnings to the wrong inbox"),
];

const CONDITIONS: [(&str, &str); S2_LEN] = [
    ("under shard rebalance", "while partitions are moving"),
    (
        "during a cold replay",
        "when history is re-read from scratch",
    ),
    ("when the vault rotates", "as secrets are cycled"),
    ("past the spill threshold", "once it starts writing to disk"),
    ("on a partial restore", "when only some state comes back"),
    (
        "while a follower catches up",
        "as a lagging copy is brought current",
    ),
    ("at epoch rollover", "when the term boundary passes"),
    (
        "under sustained backpressure",
        "while the pipeline stays saturated",
    ),
    (
        "during lens calibration",
        "while the optics are being trued",
    ),
    (
        "during the night batch",
        "when the overnight schedule peaks",
    ),
    (
        "past its recalibration due date",
        "once the instrument check is overdue",
    ),
    (
        "while the archive migrates",
        "as records move to new shelves",
    ),
    (
        "during a rolling upgrade",
        "while versions change one node at a time",
    ),
    ("under clock skew", "when machines disagree about the time"),
    ("at quota exhaustion", "once the allowance runs dry"),
    (
        "behind a cold boot",
        "right after power returns from nothing",
    ),
    (
        "inside the maintenance window",
        "while planned upkeep is underway",
    ),
    (
        "across a region failover",
        "when service jumps to another site",
    ),
    ("past the retention cutoff", "once old records age out"),
    (
        "amid a thundering herd",
        "when every client returns at once",
    ),
    ("beneath peak load", "while demand is at its highest"),
    (
        "after an unclean shutdown",
        "when the stop was not graceful",
    ),
    (
        "during certificate rotation",
        "as credentials are being swapped",
    ),
    (
        "under audit sampling",
        "while the compliance spot-checks run",
    ),
    ("within the grace period", "before forgiveness runs out"),
];

const BEHAVIOURS: [(&str, &str); S1_LEN] = [
    (
        "prefers the older replica",
        "picks the staler of two copies",
    ),
    ("batches past its ceiling", "exceeds its own size limit"),
    ("re-reads the manifest", "fetches the same index repeatedly"),
    ("holds the fence open", "keeps the barrier from closing"),
    ("serialises its probes", "checks health one at a time"),
    ("rounds the window down", "shortens its own interval"),
    ("skips the warm path", "always takes the slow route"),
    (
        "retries before backing off",
        "repeats immediately on failure",
    ),
    (
        "rounds toward the anchor",
        "biases results to the reference value",
    ),
    (
        "hoards its calibration slots",
        "keeps tuning grants for itself",
    ),
    (
        "ages its stock first",
        "serves the oldest inventory before fresh arrivals",
    ),
    (
        "mirrors its rival's cadence",
        "copies the rhythm of its counterpart",
    ),
    (
        "undercounts its retries",
        "owns up to fewer attempts than occur",
    ),
    (
        "favours the loudest tenant",
        "gives the noisiest customer priority",
    ),
    ("defers its compaction", "keeps postponing the squeeze"),
    (
        "pre-warms the wrong keys",
        "readies exactly what nobody asks for",
    ),
    (
        "round-robins its failures",
        "rotates errors evenly instead of isolating them",
    ),
    ("saves its work twice", "persists every result in duplicate"),
    ("escalates too eagerly", "raises alarms at the first wobble"),
    (
        "trims from the head",
        "throws away the oldest instead of the newest",
    ),
    ("idles between batches", "rests longer than it runs"),
    (
        "recomputes the constant",
        "derives afresh what it could remember",
    ),
    (
        "mirrors stale reads",
        "serves yesterday's answers confidently",
    ),
    (
        "chases its own tail",
        "cleans up the mess its own tidying creates",
    ),
    ("polls beyond its share", "asks for updates far too often"),
];

const CAUSES: [(&str, &str); S2_LEN] = [
    (
        "the ranker keys on arrival order rather than freshness",
        "ordering is decided by when things showed up",
    ),
    (
        "the budget is computed before the drain, not after",
        "the allowance is calculated too early",
    ),
    (
        "the manifest cache is per-thread and never shared",
        "each worker keeps a private copy of the index",
    ),
    (
        "the fence release rides the same lock as the write",
        "the barrier and the update contend for one mutex",
    ),
    (
        "the probe pool is sized from the old topology",
        "health checking was scaled for a layout that changed",
    ),
    (
        "integer division truncates the interval",
        "the arithmetic rounds the period down",
    ),
    (
        "the calibration constant was transcribed by hand",
        "a manually copied figure hid a typo",
    ),
    (
        "the sensor sums shadow and signal alike",
        "stray light lands in the same bucket as data",
    ),
    (
        "the quota counts retired entries",
        "the allowance includes what was archived",
    ),
    (
        "the seed never varies between runs",
        "randomness was pinned once and forgotten",
    ),
    (
        "the warm path is gated on a flag nobody sets",
        "the fast route is disabled by default",
    ),
    (
        "the backoff timer starts only after the first success",
        "waiting does not begin until something works",
    ),
    (
        "the retry queue and the dead-letter queue share a disk",
        "failed and pending work compete for the same spindle",
    ),
    (
        "the config loader keeps its first answer forever",
        "settings are read once and never again",
    ),
    (
        "the health probe tests the proxy, not the service",
        "the check watches the middleman instead of the target",
    ),
    (
        "the pool hands out connections newest-first",
        "the freshest link is always reused ahead of idle ones",
    ),
    (
        "the migration left a shadow column behind",
        "an orphaned field survives from the old layout",
    ),
    (
        "the timeout is measured from queueing, not execution",
        "waiting in line counts toward the clock",
    ),
    (
        "the metric averages away the spikes",
        "smoothing hides every burst",
    ),
    (
        "the lock is advisory and one caller ignores it",
        "the rule only binds those who choose to obey",
    ),
    (
        "the batch size was tuned for the old hardware",
        "the grouping still fits machines long gone",
    ),
    (
        "the parser accepts both spellings silently",
        "two written forms mean the same and nobody knows",
    ),
    (
        "the feature flag defaults on in exactly one environment",
        "a switch sits differently on a single machine",
    ),
    (
        "the cleanup job holds the same lock as intake",
        "housekeeping and arrivals fight over a single latch",
    ),
    (
        "the estimator never saw a leap year",
        "the forecast misses the calendar's odd day",
    ),
];

/// How often each kind gets asked about.
///
/// A stated assumption, not a measurement: nobody has data on what developers
/// actually ask their memory. Decisions dominate because "what did we settle
/// on" is the common question; Principles are rare because they are recalled
/// by being injected in the brief rather than searched for. Override with
/// `--type-mix`, and note that the report prints whatever mix produced it, so
/// a number can never be quoted without its weighting.
pub const DEFAULT_TYPE_MIX: [(Kind, u32); 5] = [
    (Kind::Decision, 35),
    (Kind::Caution, 20),
    (Kind::Insight, 20),
    (Kind::Problem, 15),
    (Kind::Principle, 10),
];

/// Resolve a per-kind question weight, defaulting to the mix above.
pub fn weight_of(mix: &[(Kind, u32)], kind: Kind) -> u32 {
    mix.iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, w)| *w)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Decision,
    Caution,
    Principle,
    Problem,
    Insight,
}

pub const KINDS: [Kind; 5] = [
    Kind::Decision,
    Kind::Caution,
    Kind::Principle,
    Kind::Problem,
    Kind::Insight,
];

impl Kind {
    pub fn node_type(self) -> NodeType {
        match self {
            Kind::Decision => NodeType::Decision,
            Kind::Caution => NodeType::Caution,
            Kind::Principle => NodeType::Principle,
            Kind::Problem => NodeType::Problem,
            Kind::Insight => NodeType::Insight,
        }
    }

    pub fn status(self) -> Option<NodeStatus> {
        match self {
            Kind::Problem => Some(NodeStatus::Open),
            _ => None,
        }
    }
}

/// How a question refers to the thing it is asking about. The split is the
/// point of the whole exercise: lexical questions are winnable by `grep`,
/// oblique ones are winnable only by meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phrasing {
    /// Reuses the subject name and the fact's own wording.
    Lexical,
    /// Names the subject, rewords everything else.
    Paraphrase,
    /// Never names the subject, and describes it entirely in paraphrase.
    Oblique,
}

pub const PHRASINGS: [Phrasing; 3] = [Phrasing::Lexical, Phrasing::Paraphrase, Phrasing::Oblique];

#[derive(Debug, Clone, Serialize)]
pub struct Question {
    pub text: String,
    pub phrasing: Phrasing,
    /// Key of the fact that answers it; `None` = deliberately unanswerable.
    pub gold: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fact {
    pub key: String,
    pub kind: Kind,
    pub subject: String,
    pub title: String,
    pub body: String,
    /// The substring a correct free-text answer must contain — the online
    /// half grades on this and never needs a judge.
    pub answer: String,
    /// The fact's verb phrase, already conjugated for its subject ("loses its
    /// cursor"). Kept so restatements can be built as grammatical sentences —
    /// an NLI model reading "is known to loses its cursor" is being scored on
    /// our bad English rather than on its own judgement.
    pub predicate: String,
    /// Empty for distractors — they are written to the graph and never asked
    /// about.
    pub questions: Vec<Question>,
    /// False = pure noise. Distractors exist so that recall is measured
    /// against a realistically crowded graph instead of one where every stored
    /// fact is also a right answer to something.
    pub tested: bool,
    /// The oblique question this fact *would* answer, tested or not. Held so a
    /// test can assert that no distractor is a legitimate answer to a tested
    /// question — the property that makes a miss a real miss.
    pub oblique_key: String,
    /// Set when this fact's subject name is one syllable from another fact's,
    /// which is how we measure whether ranking survives near-collisions.
    pub twin_of: Option<String>,
    /// Plausible file paths, because the embed composition covers code_refs
    /// and real nodes carry a median of one.
    pub code_refs: Vec<String>,
    /// Days before "now" this fact is written as created. Zero for the whole
    /// regular corpus; only supersession chains backdate their retired
    /// generations, so the flat-store ablation can ask whether recency alone
    /// would have picked the head.
    pub backdate_days: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NliLabel {
    Entailment,
    Neutral,
    Contradiction,
}

impl NliLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            NliLabel::Entailment => "entailment",
            NliLabel::Neutral => "neutral",
            NliLabel::Contradiction => "contradiction",
        }
    }
}

/// A labelled premise/hypothesis pair in this domain's register — the thing
/// the shipped NLI model has never been evaluated on.
#[derive(Debug, Clone, Serialize)]
pub struct Pair {
    pub premise: String,
    pub hypothesis: String,
    pub gold: NliLabel,
}

/// One ADR-shaped supersession chain: the same subject decided several times,
/// each generation replacing the last. Only the head is current; the retired
/// generations exist so the harness can measure what supersession is FOR —
/// the losing side leaving ambient retrieval while staying reachable through
/// the `replaces` chain.
#[derive(Debug, Clone, Serialize)]
pub struct Chain {
    /// Fact keys oldest first; the last entry is the live head.
    pub keys: Vec<String>,
    /// Questions about the CURRENT state; gold is always the head key.
    pub questions: Vec<Question>,
}

impl Chain {
    pub fn head(&self) -> &str {
        self.keys
            .last()
            .expect("a chain has at least one generation")
    }

    /// Every generation except the head — the ones supersession retires.
    pub fn retired(&self) -> &[String] {
        &self.keys[..self.keys.len() - 1]
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Corpus {
    pub seed: u64,
    pub facts: Vec<Fact>,
    /// Questions about subjects that were never written. The control arm:
    /// a retriever that answers these confidently is the failure mode nobody
    /// measures.
    pub unanswerable: Vec<Question>,
    /// The invented subjects those questions ask about — guaranteed absent
    /// from every fact.
    pub phantom_subjects: Vec<String>,
    pub pairs: Vec<Pair>,
    /// The graph itself. Without these the corpus measures a graph memory on
    /// a graph with no edges, which is what the first version of this harness
    /// did.
    pub edges: Vec<GeneratedEdge>,
    /// Supersession chains (empty for the regular corpus). Kept apart from
    /// `edges` on purpose: `replaces` mutates node state at write time, and
    /// the invariant that `edges` never does is load-bearing for every recall
    /// number. The chains mode wants exactly that mutation, deliberately.
    pub chains: Vec<Chain>,
}

/// One sentence-shaped link between two generated facts.
#[derive(Debug, Clone, Serialize)]
pub struct GeneratedEdge {
    pub from: String,
    pub to: String,
    pub verb: &'static str,
}

impl Corpus {
    pub fn questions(&self) -> impl Iterator<Item = &Question> {
        self.facts.iter().flat_map(|f| f.questions.iter())
    }

    /// Facts anything is ever asked about.
    pub fn tested(&self) -> usize {
        self.facts.iter().filter(|f| f.tested).count()
    }

    /// Facts written to the graph purely as noise.
    pub fn distractors(&self) -> usize {
        self.facts.len() - self.tested()
    }

    pub fn fact(&self, key: &str) -> Option<&Fact> {
        self.facts.iter().find(|f| f.key == key)
    }

    /// Every fact as one markdown record — the flat-file arms read this.
    pub fn flat_file(&self) -> String {
        let mut out = String::new();
        for f in &self.facts {
            out.push_str(&format!("## {}\n{}\n\n", f.title, f.body));
        }
        out
    }

    pub fn records(&self) -> Vec<(String, String)> {
        self.facts
            .iter()
            .map(|f| (f.key.clone(), format!("## {}\n{}", f.title, f.body)))
            .collect()
    }
}

fn coined(idx: usize) -> String {
    let o = ONSET[idx % ONSET.len()];
    let m = MID[(idx / ONSET.len()) % MID.len()];
    let c = CODA[(idx / (ONSET.len() * MID.len())) % CODA.len()];
    let mut s = format!("{o}{m}{c}");
    let head = s.remove(0).to_uppercase().to_string();
    format!("{head}{s}")
}

/// Same onset and mid, next coda — a name a ranker can plausibly confuse.
fn twin_index(idx: usize) -> usize {
    let stride = ONSET.len() * MID.len();
    let coda = (idx / stride) % CODA.len();
    idx - coda * stride + ((coda + 1) % CODA.len()) * stride
}

/// Hands out invented names, never twice.
struct Names {
    order: Vec<usize>,
    cursor: usize,
    used: HashSet<usize>,
}

impl Names {
    fn new(rng: &mut Rng) -> Self {
        let mut order: Vec<usize> = (0..ONSET.len() * MID.len() * CODA.len()).collect();
        rng.shuffle(&mut order);
        Self {
            order,
            cursor: 0,
            used: HashSet::new(),
        }
    }

    fn next(&mut self) -> usize {
        while self.cursor < self.order.len() {
            let idx = self.order[self.cursor];
            self.cursor += 1;
            if self.used.insert(idx) {
                return idx;
            }
        }
        panic!("exhausted the invented-name space");
    }

    /// The near-collision partner of an already-issued name, if it is free.
    fn twin_of(&mut self, idx: usize) -> Option<usize> {
        let t = twin_index(idx);
        self.used.insert(t).then_some(t)
    }
}

struct Vocab {
    components: Vec<usize>,
    a: Vec<usize>,
    b: Vec<usize>,
    order: Vec<usize>,
}

impl Vocab {
    fn new(rng: &mut Rng) -> Self {
        let mut components: Vec<usize> = (0..COMPONENTS.len()).collect();
        let mut a: Vec<usize> = (0..S1_LEN).collect();
        let mut b: Vec<usize> = (0..S2_LEN).collect();
        let mut order: Vec<usize> = (0..MAX_PER_KIND).collect();
        rng.shuffle(&mut components);
        rng.shuffle(&mut a);
        rng.shuffle(&mut b);
        rng.shuffle(&mut order);
        Self {
            components,
            a,
            b,
            order,
        }
    }

    /// Mixed-radix slots for the j-th fact of a kind: unique while
    /// `j < MAX_PER_KIND`.
    ///
    /// `j` is permuted first, and that permutation is load-bearing. Raw
    /// mixed radix walks slot space in order, so tested facts (the first
    /// block of indices) would occupy one contiguous region and distractors
    /// another — making the noise systematically unlike the signal, and
    /// easier to rank against than a real neighbour would be. A fixed
    /// permutation scatters distractors through the same space while keeping
    /// each tested fact's slots identical however many distractors follow it.
    fn slots(&self, j: usize) -> (usize, usize, usize) {
        let j = self.order[j % MAX_PER_KIND];
        let c = COMPONENTS.len();
        (
            self.components[j % c],
            self.a[(j / c) % S1_LEN],
            self.b[(j / (c * S1_LEN)) % S2_LEN],
        )
    }
}

/// Build a graph of `tested + distractors` facts (spread evenly across the
/// five kinds), the questions for the tested ones, a matching set of
/// unanswerable questions, and the labelled NLI pairs.
///
/// Distractors are written to the graph and never asked about. They cannot
/// contradict a tested fact and cannot answer a tested question: every fact
/// gets its own invented subject, and the mixed-radix slot enumeration simply
/// continues past the tested range, so no distractor ever repeats a tested
/// fact's (component, descriptor, qualifier) triple. Contradiction lives only
/// in `pairs`, where it is deliberate and labelled.
pub fn corpus(tested: usize, distractors: usize, seed: u64) -> Corpus {
    corpus_with(tested, distractors, seed, &Profile::default())
}

/// As `corpus`, against an explicit structural profile — the knob that asks
/// "what if our notes were shaped differently?".
pub fn corpus_with(tested: usize, distractors: usize, seed: u64, profile: &Profile) -> Corpus {
    corpus_full(tested, distractors, seed, profile, &DEFAULT_TYPE_MIX)
}

/// The full form: as `corpus_with`, plus the per-kind question weighting.
///
/// Facts stay evenly spread across kinds — the graph holds what it holds — but
/// the *questions* follow the mix, so a run reflects what gets asked rather
/// than what gets stored.
pub fn corpus_full(
    tested: usize,
    distractors: usize,
    seed: u64,
    profile: &Profile,
    type_mix: &[(Kind, u32)],
) -> Corpus {
    corpus_impl(tested, distractors, seed, profile, type_mix, 0, 0)
}

/// As `corpus_full`, plus `n_chains` ADR-shaped supersession chains of
/// `chain_len` generations each. Chain generations are written to the graph
/// as untested facts (never counted by the regular metrics); the questions
/// about their CURRENT state live on `Corpus::chains`.
pub fn corpus_chained(
    tested: usize,
    distractors: usize,
    seed: u64,
    profile: &Profile,
    type_mix: &[(Kind, u32)],
    n_chains: usize,
    chain_len: usize,
) -> Corpus {
    assert!(chain_len >= 2, "a chain needs something to supersede");
    corpus_impl(
        tested,
        distractors,
        seed,
        profile,
        type_mix,
        n_chains,
        chain_len,
    )
}

fn corpus_impl(
    tested: usize,
    distractors: usize,
    seed: u64,
    profile: &Profile,
    type_mix: &[(Kind, u32)],
    n_chains: usize,
    chain_len: usize,
) -> Corpus {
    let mut rng = Rng::new(seed);
    let vocab = Vocab::new(&mut rng);
    let mut names = Names::new(&mut rng);

    let total = tested + distractors;
    let mut facts: Vec<Fact> = Vec::with_capacity(total);
    let mut allocated: Vec<usize> = Vec::with_capacity(total);

    for i in 0..total {
        let kind = KINDS[i % KINDS.len()];
        let j = i / KINDS.len();
        assert!(
            j < MAX_PER_KIND,
            "{total} facts exceeds unique slot combinations ({} max)",
            MAX_PER_KIND * KINDS.len()
        );

        // Every 7th fact is deliberately named one syllable from an earlier
        // one, so the report can separate "ranking works" from "ranking works
        // because the names were all far apart". If that name is already
        // taken the fact simply gets a fresh one — a twin nobody can confuse
        // is not worth a collision.
        let source = (i >= 7 && i.is_multiple_of(7)).then(|| i - 7);
        let (name_idx, twin_of) = match source.and_then(|src| {
            names
                .twin_of(allocated[src])
                .map(|idx| (idx, format!("f{src:04}")))
        }) {
            Some((idx, src_key)) => (idx, Some(src_key)),
            None => (names.next(), None),
        };
        allocated.push(name_idx);

        let (c, s1, s2) = vocab.slots(j);
        let component = COMPONENTS[c];
        let subject = format!("{} {component}", coined(name_idx));
        let slots = Slots {
            component,
            s1,
            s2,
            j,
        };
        facts.push(build(
            kind,
            format!("f{i:04}"),
            subject,
            slots,
            twin_of,
            i < tested,
            profile,
            i as u64,
        ));
    }

    apply_type_mix(&mut facts, type_mix, seed);

    // Chains are generated BEFORE the controls so a phantom subject can never
    // collide with a chain's — the same `names` supply hands out both.
    // Base edges and NLI pairs are built over the pre-chain facts only:
    // chain generations deliberately share a subject, and letting `pairs`
    // draw a "neutral" partner from the same subject would mislabel it.
    let pairs = pairs(&facts, &mut rng);
    let edges = edges(&facts, profile, &mut rng);
    let chains = chains(
        &mut facts,
        n_chains,
        chain_len,
        tested + distractors,
        &vocab,
        &mut names,
        profile,
    );

    let (unanswerable, phantom_subjects) = controls(tested, &mut names);

    Corpus {
        seed,
        facts,
        unanswerable,
        phantom_subjects,
        pairs,
        edges,
        chains,
    }
}

/// Generate the supersession chains. Each chain claims a fresh invented
/// subject and a Decision slot triple past the range the regular facts used,
/// so no chain's oblique question is answerable by anything else — and each
/// generation re-decides the same parameter to a DIFFERENT value, which is
/// what makes a retired generation genuinely wrong rather than merely stale.
fn chains(
    facts: &mut Vec<Fact>,
    n_chains: usize,
    chain_len: usize,
    total: usize,
    vocab: &Vocab,
    names: &mut Names,
    profile: &Profile,
) -> Vec<Chain> {
    if n_chains == 0 {
        return Vec::new();
    }
    // Decision facts consumed slot ordinals 0..ceil(total/KINDS); continue
    // past them so the mixed-radix triples stay unique by construction.
    let used = total.div_ceil(KINDS.len());
    assert!(
        used + n_chains <= MAX_PER_KIND,
        "{n_chains} chains exceed the free Decision slot space"
    );

    let mut out = Vec::with_capacity(n_chains);
    for ci in 0..n_chains {
        let j = used + ci;
        let (c, s1, s2) = vocab.slots(j);
        let component = COMPONENTS[c];
        let subject = format!("{} {component}", coined(names.next()));
        let (param, unit, param_desc) = PARAMS[s1];
        let (constraint, constraint_desc) = CONSTRAINTS[s2];

        let mut keys = Vec::with_capacity(chain_len);
        for g in 0..chain_len {
            // 89 and 977 are coprime, so every generation lands on its own
            // value — a retired generation that repeated the head's number
            // would be a right answer wearing the wrong key.
            let value = 3 + (j * 13 + (g + 1) * 89) % 977;
            let key = format!("c{ci:03}g{g}");
            let ord = (total + ci * chain_len + g) as u64;
            let mix = ord.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let title = extend_title(format!("{subject} uses a {param} of {value} {unit}"), mix);
            let body = pad_body(
                format!(
                    "Chosen deliberately: {constraint}, so anything larger re-enters a window \
                     that has already closed."
                ),
                &subject,
                component,
                profile.body_chars.sample(mix),
                mix,
            );
            facts.push(Fact {
                key: key.clone(),
                kind: Kind::Decision,
                subject: subject.clone(),
                title,
                body,
                answer: format!("{value} {unit}"),
                predicate: format!("uses a {param} of {value} {unit}"),
                questions: Vec::new(),
                tested: false,
                // Suffixed per generation: within a chain the triple is shared
                // on purpose, and the corpus-wide uniqueness set must not see
                // that as two facts answering one question.
                oblique_key: format!(
                    "which {component} picked {param_desc} around the fact that \
                     {constraint_desc}? [{key}]"
                ),
                twin_of: None,
                code_refs: code_refs(component, profile.code_refs.sample(mix >> 3), mix),
                // The head is written "now"; each earlier generation a month
                // further back, so the flat-store ablation can ask whether
                // recency alone would have picked the head.
                backdate_days: ((chain_len - 1 - g) as u64) * 30,
            });
            keys.push(key);
        }

        let head = keys.last().expect("chain_len >= 2").clone();
        let questions = vec![
            Question {
                text: format!("{subject} {param}"),
                phrasing: Phrasing::Lexical,
                gold: Some(head.clone()),
            },
            Question {
                text: format!("what {param} did we settle on for the {subject}?"),
                phrasing: Phrasing::Paraphrase,
                gold: Some(head.clone()),
            },
            Question {
                text: format!(
                    "which {component} picked {param_desc} around the fact that \
                     {constraint_desc}?"
                ),
                phrasing: Phrasing::Oblique,
                gold: Some(head),
            },
        ];
        out.push(Chain { keys, questions });
    }
    out
}

/// Verbs the generator will emit, with the kinds each one legally joins.
///
/// `replaces` and `conflicts-with` are deliberately absent even though the
/// real graph carries a few: both mutate node state at write time (archival
/// and trust demotion), which would silently remove gold facts from search and
/// confound every recall number. Together they are 2.7% of real edges — a
/// cheap thing to give up for a measurement that means something.
const EDGE_RULES: [(&str, Kind, Kind); 6] = [
    ("because", Kind::Decision, Kind::Principle),
    ("about", Kind::Caution, Kind::Decision),
    ("about", Kind::Problem, Kind::Decision),
    ("builds-on", Kind::Insight, Kind::Decision),
    ("answers", Kind::Insight, Kind::Problem),
    ("needs", Kind::Decision, Kind::Decision),
];

/// Roughly the share of edges joining facts about the same component.
///
/// Real edges connect things that are topically related, and in this corpus
/// the component is the topic handle. Linking only across components would be
/// unrealistically pessimistic (a neighbour that shares nothing with the query
/// never ranks, so the graph could never help); linking only within one would
/// be unrealistically flattering. The remainder are cross-component — a
/// Principle cited from somewhere else entirely — which is the case where the
/// edge carries information no embedding could infer.
const SAME_COMPONENT_SHARE: u64 = 70;

fn component_of(f: &Fact) -> &str {
    f.subject.split_once(' ').map(|(_, c)| c).unwrap_or("")
}

fn edges(facts: &[Fact], profile: &Profile, rng: &mut Rng) -> Vec<GeneratedEdge> {
    let target = (facts.len() as f64 * profile.edges_per_node) as usize;
    if target == 0 || facts.is_empty() {
        return Vec::new();
    }
    // A slice of the corpus stays deliberately unlinked, matching the ~4% of
    // real nodes that never got connected to anything.
    let isolated: HashSet<usize> = (0..facts.len())
        .filter(|i| (*i as f64) < facts.len() as f64 * profile.isolated)
        .collect();

    let by_kind = |k: Kind| -> Vec<usize> {
        (0..facts.len())
            .filter(|i| facts[*i].kind == k && !isolated.contains(i))
            .collect()
    };
    let pools: Vec<(&str, Vec<usize>, Vec<usize>)> = EDGE_RULES
        .iter()
        .map(|(verb, from, to)| (*verb, by_kind(*from), by_kind(*to)))
        .collect();

    let mut out = Vec::with_capacity(target);
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut attempts = 0;
    while out.len() < target && attempts < target * 20 {
        attempts += 1;
        let (verb, from_pool, to_pool) = &pools[rng.below(pools.len())];
        if from_pool.is_empty() || to_pool.is_empty() {
            continue;
        }
        let a = from_pool[rng.below(from_pool.len())];
        let want_same = rng.below(100) < SAME_COMPONENT_SHARE as usize;
        let comp = component_of(&facts[a]);
        let candidates: Vec<usize> = to_pool
            .iter()
            .copied()
            .filter(|b| *b != a && (component_of(&facts[*b]) == comp) == want_same)
            .collect();
        let Some(&b) = candidates.get(rng.below(candidates.len().max(1))) else {
            continue;
        };
        if !seen.insert((a.min(b), a.max(b))) {
            continue;
        }
        out.push(GeneratedEdge {
            from: facts[a].key.clone(),
            to: facts[b].key.clone(),
            verb,
        });
    }
    out
}

/// The resolved vocabulary slots for one fact: which component it is about,
/// two descriptor indices, and its ordinal within the kind (which sets the
/// numeric value a Decision commits to).
struct Slots<'a> {
    component: &'a str,
    s1: usize,
    s2: usize,
    j: usize,
}

#[allow(clippy::too_many_arguments)]
fn build(
    kind: Kind,
    key: String,
    subject: String,
    slots: Slots<'_>,
    twin_of: Option<String>,
    tested: bool,
    profile: &Profile,
    ord: u64,
) -> Fact {
    let Slots {
        component,
        s1,
        s2,
        j,
    } = slots;

    let (title, body, answer, predicate, questions) = match kind {
        Kind::Decision => {
            let (param, unit, param_desc) = PARAMS[s1];
            let (constraint, constraint_desc) = CONSTRAINTS[s2];
            let value = 3 + (j * 13) % 977;
            (
                format!("{subject} uses a {param} of {value} {unit}"),
                format!(
                    "Chosen deliberately: {constraint}, so anything larger re-enters a window \
                     that has already closed."
                ),
                format!("{value} {unit}"),
                format!("uses a {param} of {value} {unit}"),
                vec![
                    (Phrasing::Lexical, format!("{subject} {param}")),
                    (
                        Phrasing::Paraphrase,
                        format!("what {param} did we settle on for the {subject}?"),
                    ),
                    (
                        Phrasing::Oblique,
                        format!(
                            "which {component} picked {param_desc} around the fact that \
                             {constraint_desc}?"
                        ),
                    ),
                ],
            )
        }
        Kind::Caution => {
            let (failure, failure_desc) = FAILURES[s1];
            let (trigger, trigger_desc) = TRIGGERS[s2];
            (
                format!("{subject} {failure} when {trigger}"),
                "Found the hard way. The window is narrow enough that a healthy run never \
                 shows it, which is exactly why it survived review."
                    .to_string(),
                failure.to_string(),
                failure.to_string(),
                vec![
                    (Phrasing::Lexical, format!("{subject} {failure}")),
                    (
                        Phrasing::Paraphrase,
                        format!("what goes wrong with the {subject}?"),
                    ),
                    (
                        Phrasing::Oblique,
                        format!("which {component} {failure_desc} once {trigger_desc}?"),
                    ),
                ],
            )
        }
        Kind::Principle => {
            let (forbidden, forbidden_desc) = FORBIDDEN[s1];
            let (reason, reason_desc) = REASONS[s2];
            (
                format!("{subject} must never {forbidden}"),
                format!("Not negotiable: {reason}."),
                forbidden.to_string(),
                format!("must never {forbidden}"),
                vec![
                    (Phrasing::Lexical, format!("{subject} {forbidden}")),
                    (
                        Phrasing::Paraphrase,
                        format!("is it acceptable to {forbidden} in the {subject}?"),
                    ),
                    (
                        Phrasing::Oblique,
                        format!(
                            "which {component} is barred from {forbidden_desc} because \
                             {reason_desc}?"
                        ),
                    ),
                ],
            )
        }
        Kind::Problem => {
            let (symptom, symptom_desc) = SYMPTOMS[s1];
            let (condition, condition_desc) = CONDITIONS[s2];
            (
                format!("{subject} {symptom} {condition}"),
                format!(
                    "Still open. Reproduces roughly one run in six, and only {condition}, so the \
                     usual smoke pass never catches it."
                ),
                symptom.to_string(),
                symptom.to_string(),
                vec![
                    (Phrasing::Lexical, format!("{subject} {symptom}")),
                    (
                        Phrasing::Paraphrase,
                        format!("what is still broken in the {subject}?"),
                    ),
                    (
                        Phrasing::Oblique,
                        format!("which {component} {symptom_desc} {condition_desc}?"),
                    ),
                ],
            )
        }
        Kind::Insight => {
            let (behaviour, behaviour_desc) = BEHAVIOURS[s1];
            let (cause, cause_desc) = CAUSES[s2];
            (
                format!("{subject} {behaviour} because {cause}"),
                format!("Worked this out while tracing a report: {cause}."),
                cause.to_string(),
                behaviour.to_string(),
                vec![
                    (Phrasing::Lexical, format!("{subject} {behaviour}")),
                    (
                        Phrasing::Paraphrase,
                        format!("what explains why the {subject} {behaviour}?"),
                    ),
                    (
                        Phrasing::Oblique,
                        format!("which {component} {behaviour_desc} because {cause_desc}?"),
                    ),
                ],
            )
        }
    };

    let oblique_key = questions
        .iter()
        .find(|(p, _)| *p == Phrasing::Oblique)
        .map(|(_, text)| text.clone())
        .expect("every kind generates an oblique question");

    // A distractor is written to the graph but never asked about.
    let questions = if tested {
        questions
            .into_iter()
            .map(|(phrasing, text)| Question {
                text,
                phrasing,
                gold: Some(key.clone()),
            })
            .collect()
    } else {
        Vec::new()
    };

    let mix = ord.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let title = extend_title(title, mix);
    let body = pad_body(
        body,
        &subject,
        component,
        profile.body_chars.sample(mix),
        mix,
    );
    let code_refs = code_refs(component, profile.code_refs.sample(mix >> 3), mix);

    Fact {
        key,
        kind,
        subject,
        title,
        body,
        answer,
        predicate,
        questions,
        tested,
        oblique_key,
        twin_of,
        code_refs,
        backdate_days: 0,
    }
}

/// Real titles run to a median of 89 characters and carry a trailing clause
/// that says why the note exists. The tails below deliberately share no
/// vocabulary with any paraphrase column, so extending a title cannot make an
/// oblique question lexically findable — the property the whole oblique
/// measurement rests on, and one a test enforces.
const TITLE_TAILS: [&str; 8] = [
    " — surfaced on the third occurrence, not the one everybody remembers",
    " — corroborated independently by two separate investigations",
    ", and no surrounding layer makes up for it",
    ", which is why it is recorded rather than quietly patched around",
    " — the workaround proved costlier than simply honouring the rule",
    ", and it only shows up under sustained production traffic",
    " — reproduced in staging by somebody who did not believe it",
    ", so the engineer who follows is spared the same expense",
];

fn extend_title(title: String, mix: u64) -> String {
    format!("{title}{}", TITLE_TAILS[(mix % 8) as usize])
}

/// Sentences that add realistic bulk without adding retrievable claims. They
/// name only the fact's own subject and component, never another fact's slot
/// vocabulary — a filler sentence containing some other fact's answer would
/// manufacture a false positive and quietly corrupt every recall number.
const FILLER: [&str; 12] = [
    "Recorded only once three separate sightings had accumulated, at which point bad luck stopped looking plausible.",
    "The {component} is maintained by the same group that owns its callers, and they have approved this phrasing.",
    "Kept deliberately narrow, since broadening it would sweep in neighbouring situations that behave quite unlike this one.",
    "A ticket for this exists somewhere in the tracker, but it carries far less context than this note.",
    "Somebody will eventually want to promote this into a general rule. That experiment has already run its course, unhappily.",
    "Figures quoted here were collected in staging under representative traffic, not in a controlled harness.",
    "Revisited at the following planning session and carried forward without amendment.",
    "This note exists because working the reasoning out was costly while stating the conclusion is trivial.",
    "Arriving here mid-investigation? Start from the {component} and work outward from there.",
    "Two engineers examined it separately, roughly a month apart, and landed in precisely the same place.",
    "The obvious alternative got weighed and turned down on operational grounds rather than technical ones.",
    "{subject} remains the only place this has ever been observed, which is why it is filed against it directly.",
];

fn pad_body(core: String, subject: &str, component: &str, target: usize, mix: u64) -> String {
    let mut out = core;
    let mut i = (mix >> 16) as usize;
    while out.chars().count() < target {
        let f = FILLER[i % FILLER.len()]
            .replace("{component}", component)
            .replace("{subject}", subject);
        out.push(' ');
        out.push_str(&f);
        i += 1;
        // One pass through the pool is enough bulk for the longest real body;
        // repeating it would make every long note read identically.
        if i > (mix >> 16) as usize + FILLER.len() {
            break;
        }
    }
    out
}

fn code_refs(component: &str, n: usize, mix: u64) -> Vec<String> {
    let slug: String = component.replace(' ', "_");
    const FILES: [&str; 4] = ["mod.rs", "state.rs", "handler.rs", "config.rs"];
    (0..n)
        .map(|k| {
            let f = FILES[((mix >> (k * 3)) % 4) as usize];
            format!("src/{slug}/{f}")
        })
        .collect()
}

/// Thin the questions of over-represented kinds until the surviving mix
/// matches the requested weighting.
///
/// Questions are dropped rather than duplicated: asking the same question
/// twice would double-count one retrieval outcome and quietly narrow the
/// confidence of every number derived from it.
fn apply_type_mix(facts: &mut [Fact], mix: &[(Kind, u32)], seed: u64) {
    let tested: Vec<usize> = (0..facts.len()).filter(|i| facts[*i].tested).collect();
    if tested.is_empty() {
        return;
    }
    let counts: Vec<(Kind, usize)> = KINDS
        .iter()
        .map(|k| (*k, tested.iter().filter(|i| facts[**i].kind == *k).count()))
        .collect();

    // Proportional thinning: the most-asked kind keeps every question it has,
    // and each other kind keeps its share relative to that one.
    let max_weight = KINDS.iter().map(|k| weight_of(mix, *k)).max().unwrap_or(0);
    if max_weight == 0 {
        return;
    }

    let mut rng = Rng::new(seed ^ 0xA5A5_A5A5);
    for k in KINDS {
        let n = counts
            .iter()
            .find(|(c, _)| *c == k)
            .map(|(_, n)| *n)
            .unwrap_or(0);
        let keep = ((weight_of(mix, k) as f64 / max_weight as f64) * n as f64).round() as usize;
        let keep = keep.min(n);
        let mut of_kind: Vec<usize> = tested
            .iter()
            .copied()
            .filter(|i| facts[*i].kind == k)
            .collect();
        rng.shuffle(&mut of_kind);
        for i in of_kind.into_iter().skip(keep) {
            facts[i].questions.clear();
        }
    }
}

/// Questions shaped like the answerable ones, about subjects that were never
/// written. Any hit here is a false positive.
fn controls(tested: usize, names: &mut Names) -> (Vec<Question>, Vec<String>) {
    let n = (tested / 4).max(1);
    let mut questions = Vec::with_capacity(n);
    let mut subjects = Vec::with_capacity(n);

    for i in 0..n {
        let component = COMPONENTS[i % COMPONENTS.len()];
        let subject = format!("{} {component}", coined(names.next()));
        let phrasing = PHRASINGS[i % PHRASINGS.len()];
        questions.push(Question {
            text: match phrasing {
                Phrasing::Lexical => format!("{subject} retry budget"),
                Phrasing::Paraphrase => format!("what did we decide about the {subject}?"),
                Phrasing::Oblique => format!("who owns the {subject} these days?"),
            },
            phrasing,
            gold: None,
        });
        subjects.push(subject);
    }
    (questions, subjects)
}

/// One contradiction, one entailment and one neutral pair per tested fact —
/// ground truth by construction, so scoring an NLI model costs nothing. The
/// neutral partner may be a distractor: two facts about different invented
/// subjects are unrelated, which is exactly what neutral means.
fn pairs(facts: &[Fact], rng: &mut Rng) -> Vec<Pair> {
    let mut out = Vec::with_capacity(facts.len() * 3);
    for (i, f) in facts.iter().enumerate().filter(|(_, f)| f.tested) {
        let (contra, entail) = restatements(f, i);
        out.push(Pair {
            premise: f.title.clone(),
            hypothesis: contra,
            gold: NliLabel::Contradiction,
        });
        out.push(Pair {
            premise: f.title.clone(),
            hypothesis: entail,
            gold: NliLabel::Entailment,
        });
        if facts.len() > 1 {
            let mut other = rng.below(facts.len());
            if other == i {
                other = (other + 1) % facts.len();
            }
            out.push(Pair {
                premise: f.title.clone(),
                hypothesis: facts[other].title.clone(),
                gold: NliLabel::Neutral,
            });
        }
    }
    out
}

/// (contradicting restatement, entailed restatement) for one fact.
fn restatements(f: &Fact, salt: usize) -> (String, String) {
    let subject = &f.subject;
    match f.kind {
        Kind::Decision => {
            // Same subject and parameter, a different value: the cleanest
            // contradiction there is, and the one similarity alone cannot see.
            let value = &f.answer;
            let altered = alter_number(value, salt);
            (
                format!("{subject} is configured with {altered}"),
                format!("The agreed setting for the {subject} is {value}."),
            )
        }
        Kind::Caution => (
            format!("{subject} never {}, whatever happens", f.predicate),
            format!("In production, {subject} {} without warning.", f.predicate),
        ),
        Kind::Principle => (
            format!("{subject} may freely {}", f.answer),
            format!("It is forbidden for the {subject} to {}.", f.answer),
        ),
        Kind::Problem => (
            format!("{subject} no longer {}", f.predicate),
            format!(
                "There is an open issue where the {subject} {}.",
                f.predicate
            ),
        ),
        Kind::Insight => (
            format!(
                "It is not true that {subject} {} because {}",
                f.predicate, f.answer
            ),
            format!(
                "The reason the {subject} {} is that {}.",
                f.predicate, f.answer
            ),
        ),
    }
}

fn alter_number(answer: &str, salt: usize) -> String {
    match answer.split_once(' ') {
        Some((num, unit)) => match num.parse::<usize>() {
            Ok(v) => format!("{} {unit}", v + 1 + salt % 40),
            Err(_) => format!("something other than {answer}"),
        },
        None => format!("something other than {answer}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_for_a_seed() {
        let a = corpus(40, 80, 42);
        let b = corpus(40, 80, 42);
        let c = corpus(40, 80, 43);
        assert_eq!(
            a.facts.iter().map(|f| &f.title).collect::<Vec<_>>(),
            b.facts.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
        assert_ne!(
            a.facts.iter().map(|f| &f.title).collect::<Vec<_>>(),
            c.facts.iter().map(|f| &f.title).collect::<Vec<_>>(),
            "a different seed must produce a different corpus"
        );
    }

    #[test]
    fn subjects_and_titles_are_unique() {
        for size in [50, 200, 800] {
            let c = corpus(size, size * 2, 1);
            let n = c.facts.len();
            let subjects: HashSet<_> = c.facts.iter().map(|f| &f.subject).collect();
            let titles: HashSet<_> = c.facts.iter().map(|f| &f.title).collect();
            assert_eq!(subjects.len(), n, "subject collision at {size}");
            assert_eq!(titles.len(), n, "title collision at {size}");
        }
    }

    #[test]
    fn oblique_questions_never_name_their_subject() {
        let c = corpus(120, 240, 5);
        for f in &c.facts {
            let name = f.subject.split(' ').next().unwrap();
            for q in f
                .questions
                .iter()
                .filter(|q| q.phrasing == Phrasing::Oblique)
            {
                assert!(
                    !q.text.contains(name),
                    "oblique question leaked the subject name: {}",
                    q.text
                );
            }
        }
    }

    #[test]
    fn no_fact_can_answer_another_facts_oblique_question() {
        // The load-bearing invariant. Two facts sharing an oblique descriptor
        // would make the question genuinely ambiguous, and a retriever that
        // returned the other one would be scored as wrong for being right.
        // Distractors are included: a distractor that answers a tested
        // question is not a distractor, it is a second correct answer.
        for size in [50, 200, 800] {
            let c = corpus(size, size * 2, 9);
            let mut seen = HashSet::new();
            for f in &c.facts {
                assert!(
                    seen.insert(f.oblique_key.clone()),
                    "two facts answer the same oblique question at size {size}: {}",
                    f.oblique_key
                );
            }
        }
    }

    #[test]
    fn oblique_questions_share_almost_no_words_with_their_fact() {
        // The whole reason the oblique column means anything: if it shared the
        // fact's vocabulary, grep would win it and the metric would be noise.
        // The component ("shard router") is the one handle an oblique question
        // is allowed to share — it names the category, not the fact, and it is
        // the same for every eighth fact in the corpus.
        let c = corpus(100, 200, 7);
        for f in &c.facts {
            let component: HashSet<String> = f
                .subject
                .split(' ')
                .skip(1)
                .map(|w| w.to_lowercase())
                .collect();
            // Connectives are structural, not content — both a fact and a
            // question about it will say "because".
            const CONNECTIVES: [&str; 8] = [
                "because", "which", "while", "after", "before", "these", "those", "still",
            ];
            let fact_words: HashSet<String> = format!("{} {}", f.title, f.body)
                .split(|ch: char| !ch.is_alphanumeric())
                .filter(|w| w.len() > 4)
                .map(|w| w.to_lowercase())
                .filter(|w| !component.contains(w) && !CONNECTIVES.contains(&w.as_str()))
                .collect();
            for q in f
                .questions
                .iter()
                .filter(|q| q.phrasing == Phrasing::Oblique)
            {
                let shared = q
                    .text
                    .split(|ch: char| !ch.is_alphanumeric())
                    .filter(|w| w.len() > 4)
                    .map(|w| w.to_lowercase())
                    .filter(|w| fact_words.contains(w))
                    .count();
                assert!(
                    shared <= 1,
                    "oblique question shares {shared} content words with its fact:\n  {}\n  {}",
                    q.text,
                    f.title
                );
            }
        }
    }

    /// Connectives are structural and shared by any two English sentences
    /// about the same thing.
    const CONNECTIVES: [&str; 9] = [
        "because", "which", "while", "after", "before", "these", "those", "still", "there",
    ];

    fn content_words(text: &str) -> HashSet<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 4)
            .map(|w| w.to_lowercase())
            .filter(|w| !CONNECTIVES.contains(&w.as_str()))
            .collect()
    }

    #[test]
    fn filler_shares_no_vocabulary_with_any_paraphrase() {
        // The load-bearing guard behind the oblique column. Body filler exists
        // to make documents realistically long; if a filler sentence reuses a
        // paraphrase word, an oblique question becomes lexically findable and
        // the one measurement that separates meaning from keywords quietly
        // turns into noise. Checked here rather than by eye, because it was
        // by eye that thirteen collisions got in.
        let mut paraphrases: Vec<&str> = Vec::new();
        paraphrases.extend(PARAMS.iter().map(|(_, _, p)| *p));
        for table in [
            &CONSTRAINTS[..],
            &FAILURES[..],
            &TRIGGERS[..],
            &FORBIDDEN[..],
            &REASONS[..],
            &SYMPTOMS[..],
            &CONDITIONS[..],
            &BEHAVIOURS[..],
            &CAUSES[..],
        ] {
            paraphrases.extend(table.iter().map(|(_, p)| *p));
        }
        let para: HashSet<String> = paraphrases.iter().flat_map(|p| content_words(p)).collect();

        let mut leaked: Vec<String> = FILLER
            .iter()
            .chain(TITLE_TAILS.iter())
            .flat_map(|f| content_words(f))
            .filter(|w| para.contains(w))
            .collect();
        leaked.sort();
        leaked.dedup();
        assert!(
            leaked.is_empty(),
            "filler reuses paraphrase vocabulary: {leaked:?}"
        );
    }

    #[test]
    fn no_fact_body_contains_another_facts_answer() {
        // Filler that happened to contain some other fact's checkable answer
        // would manufacture a false positive in the online half and corrupt
        // every recall number in the offline half.
        let c = corpus(40, 80, 21);
        for f in &c.facts {
            let hay = format!("{} {}", f.title, f.body).to_lowercase();
            for other in c.facts.iter().filter(|o| o.key != f.key) {
                // Two facts can legitimately draw the same slot value, so an
                // identical answer is a shared vocabulary entry, not a leak.
                // (It is still a grading hazard for the online half, which is
                // why that grader also requires the subject name.)
                if other.answer == f.answer {
                    continue;
                }
                let needle = other.answer.to_lowercase();
                // Answers are phrases; single shared words are unavoidable and
                // harmless. Only a whole answer appearing verbatim is a leak.
                if needle.split_whitespace().count() > 1 {
                    assert!(
                        !hay.contains(&needle),
                        "{} contains {}'s answer {:?}",
                        f.key,
                        other.key,
                        other.answer
                    );
                }
            }
        }
    }

    #[test]
    fn generated_nodes_match_the_real_shape() {
        use crate::profile::Profile;
        let p = Profile::default();
        let c = corpus(60, 120, 22);
        let median = |mut v: Vec<usize>| {
            v.sort_unstable();
            v[v.len() / 2]
        };
        let body = median(c.facts.iter().map(|f| f.body.chars().count()).collect());
        let title = median(c.facts.iter().map(|f| f.title.chars().count()).collect());
        assert!(
            body > p.body_chars.p25 && body < p.body_chars.p75,
            "median body {body} outside the real p25..p75 ({}..{})",
            p.body_chars.p25,
            p.body_chars.p75
        );
        assert!(
            title > 60,
            "median title {title} is far below the real median of {}",
            p.title_chars.median
        );
        assert!(c.facts.iter().any(|f| !f.code_refs.is_empty()));
    }

    #[test]
    fn the_generated_graph_matches_the_real_topology() {
        use crate::profile::Profile;
        let p = Profile::default();
        let c = corpus(100, 200, 31);
        let n = c.facts.len();
        let per_node = c.edges.len() as f64 / n as f64;
        assert!(
            (per_node - p.edges_per_node).abs() < 0.25,
            "{per_node} edges per node vs a real {}",
            p.edges_per_node
        );

        let mut degree: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for e in &c.edges {
            *degree.entry(e.from.as_str()).or_default() += 1;
            *degree.entry(e.to.as_str()).or_default() += 1;
        }
        let isolated = c
            .facts
            .iter()
            .filter(|f| !degree.contains_key(f.key.as_str()))
            .count();
        let share = isolated as f64 / n as f64;
        assert!(
            share < 0.35,
            "{share} of nodes isolated — a real graph leaves about 4% unlinked"
        );

        // Every edge must read as English in the ontology's terms.
        for e in &c.edges {
            let (from, to) = (c.fact(&e.from).unwrap(), c.fact(&e.to).unwrap());
            assert!(
                EDGE_RULES
                    .iter()
                    .any(|(v, a, b)| *v == e.verb && *a == from.kind && *b == to.kind),
                "{:?} {} {:?} is not a legal triple",
                from.kind,
                e.verb,
                to.kind
            );
            assert_ne!(e.from, e.to, "no self-links");
        }
    }

    #[test]
    fn edges_never_mutate_node_state() {
        // `replaces` archives its older endpoint and `conflicts-with` demotes
        // trust. Either would remove a gold fact from search results and
        // corrupt recall without any test failing.
        let c = corpus(60, 120, 32);
        assert!(
            c.edges
                .iter()
                .all(|e| e.verb != "replaces" && e.verb != "conflicts-with"),
            "a state-mutating verb reached the corpus"
        );
    }

    #[test]
    fn edges_mix_same_and_cross_component_links() {
        let c = corpus(100, 200, 33);
        let same = c
            .edges
            .iter()
            .filter(|e| {
                component_of(c.fact(&e.from).unwrap()) == component_of(c.fact(&e.to).unwrap())
            })
            .count();
        let share = same as f64 / c.edges.len() as f64;
        assert!(
            (0.4..0.95).contains(&share),
            "same-component share {share}: all-same is flattering, all-cross is pessimistic"
        );
    }

    #[test]
    fn questions_follow_the_type_mix() {
        let c = corpus(200, 0, 41);
        let count = |k: Kind| {
            c.facts
                .iter()
                .filter(|f| f.kind == k)
                .map(|f| f.questions.len())
                .sum::<usize>() as f64
        };
        let (d, p) = (count(Kind::Decision), count(Kind::Principle));
        assert!(d > 0.0 && p > 0.0);
        let ratio = d / p;
        let want = 35.0 / 10.0;
        assert!(
            (ratio - want).abs() < 1.0,
            "decision:principle questions {ratio:.2}, mix asks for {want:.2}"
        );
        assert!(
            count(Kind::Caution) > count(Kind::Problem),
            "cautions are asked about more often than problems"
        );
    }

    #[test]
    fn phantom_subjects_appear_nowhere_in_the_corpus() {
        let c = corpus(100, 200, 2);
        let flat = c.flat_file().to_lowercase();
        assert!(!c.phantom_subjects.is_empty());
        for s in &c.phantom_subjects {
            let name = s.split(' ').next().unwrap().to_lowercase();
            assert!(
                !flat.contains(&name),
                "control question names something we wrote: {s}"
            );
        }
    }

    #[test]
    fn every_fact_carries_a_checkable_answer() {
        let c = corpus(60, 120, 4);
        for f in &c.facts {
            assert!(!f.answer.is_empty());
            let hay = format!("{} {}", f.title, f.body).to_lowercase();
            assert!(
                hay.contains(&f.answer.to_lowercase()),
                "answer '{}' is not present in its own fact",
                f.answer
            );
        }
    }

    #[test]
    fn pairs_are_balanced_and_labelled() {
        let c = corpus(30, 60, 8);
        assert_eq!(c.pairs.len(), c.tested() * 3);
        for label in [
            NliLabel::Contradiction,
            NliLabel::Entailment,
            NliLabel::Neutral,
        ] {
            assert_eq!(
                c.pairs.iter().filter(|p| p.gold == label).count(),
                c.tested()
            );
        }
    }

    #[test]
    fn distractors_are_written_but_never_asked_about() {
        let c = corpus(50, 100, 11);
        assert_eq!(c.tested(), 50);
        assert_eq!(c.distractors(), 100);
        assert_eq!(c.facts.len(), 150);
        // Questions follow the type mix, so the count is no longer 3x tested.
        assert!(
            c.questions().count() > 0 && c.questions().count() <= 50 * 3,
            "questions come only from tested facts, thinned by the type mix"
        );
        assert!(
            c.facts
                .iter()
                .filter(|f| !f.tested)
                .all(|f| f.questions.is_empty()),
            "a distractor with a question is not a distractor"
        );
        // ...but they are in the graph, so every arm has to rank past them.
        let flat = c.flat_file();
        for f in c.facts.iter().filter(|f| !f.tested) {
            assert!(
                flat.contains(&f.title),
                "distractor missing from the corpus"
            );
        }
    }

    #[test]
    fn no_distractor_contradicts_a_tested_fact() {
        // Contradiction requires talking about the same thing. Every fact owns
        // a unique invented subject, so no two facts in the graph are ever
        // about the same entity — the only contradictions in the whole corpus
        // are the ones `pairs` constructs deliberately, and those are never
        // written to the graph.
        let c = corpus(60, 120, 12);
        let subjects: HashSet<&String> = c.facts.iter().map(|f| &f.subject).collect();
        assert_eq!(subjects.len(), c.facts.len());

        let titles: HashSet<&String> = c.facts.iter().map(|f| &f.title).collect();
        for p in &c.pairs {
            if p.gold == NliLabel::Contradiction {
                assert!(
                    !titles.contains(&p.hypothesis),
                    "a constructed contradiction leaked into the graph: {}",
                    p.hypothesis
                );
            }
        }
    }

    #[test]
    fn distractors_scale_the_graph_without_scaling_the_questions() {
        // What makes a density sweep mean anything: hold the tested set fixed,
        // grow only the noise.
        let a = corpus(40, 0, 13);
        let b = corpus(40, 400, 13);
        assert_eq!(a.questions().count(), b.questions().count());
        assert_eq!(a.pairs.len(), b.pairs.len());
        assert_eq!(a.unanswerable.len(), b.unanswerable.len());
        assert!(b.flat_file().len() > a.flat_file().len() * 5);
        // The tested facts themselves must be identical, or the two runs are
        // not comparable.
        let tested = |c: &Corpus| {
            c.facts
                .iter()
                .filter(|f| f.tested)
                .map(|f| f.title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(tested(&a), tested(&b));
    }

    #[test]
    fn chains_share_a_subject_and_disagree_on_the_value() {
        let c = corpus_chained(40, 80, 15, &Profile::default(), &DEFAULT_TYPE_MIX, 6, 3);
        assert_eq!(c.chains.len(), 6);
        assert_eq!(c.facts.len(), 40 + 80 + 6 * 3);
        for ch in &c.chains {
            assert_eq!(ch.keys.len(), 3);
            assert_eq!(ch.retired().len(), 2);
            let gens: Vec<&Fact> = ch.keys.iter().map(|k| c.fact(k).unwrap()).collect();
            // One subject, three distinct answers, three distinct titles —
            // the shape of a re-decided decision rather than a duplicate.
            assert!(gens.iter().all(|f| f.subject == gens[0].subject));
            let answers: HashSet<&String> = gens.iter().map(|f| &f.answer).collect();
            assert_eq!(answers.len(), 3, "every generation re-decides the value");
            let titles: HashSet<&String> = gens.iter().map(|f| &f.title).collect();
            assert_eq!(titles.len(), 3);
            // The head is written now; every earlier generation further back.
            let head = c.fact(ch.head()).unwrap();
            assert_eq!(head.backdate_days, 0);
            for r in ch.retired() {
                assert!(c.fact(r).unwrap().backdate_days > 0);
            }
            // Chain generations are graph residents, never regular metrics
            // inputs: untested, question-less, and absent from the NLI pairs.
            for f in &gens {
                assert!(!f.tested && f.questions.is_empty());
            }
            assert_eq!(ch.questions.len(), 3);
            assert!(
                ch.questions
                    .iter()
                    .all(|q| q.gold.as_deref() == Some(ch.head()))
            );
        }
        // Chain subjects never collide with each other, the regular facts, or
        // the phantom controls.
        let subjects: HashSet<&String> = c.facts.iter().map(|f| &f.subject).collect();
        assert_eq!(subjects.len(), 40 + 80 + 6, "one subject per chain");
        for p in &c.phantom_subjects {
            assert!(!subjects.contains(p));
        }
        // And the regular corpus stays byte-identical without them.
        let plain = corpus_full(40, 80, 15, &Profile::default(), &DEFAULT_TYPE_MIX);
        assert!(plain.chains.is_empty());
        assert_eq!(
            plain.facts.iter().map(|f| &f.title).collect::<Vec<_>>(),
            c.facts[..120].iter().map(|f| &f.title).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn twins_exist_and_differ_by_one_syllable() {
        let c = corpus(60, 120, 6);
        let twins: Vec<_> = c.facts.iter().filter(|f| f.twin_of.is_some()).collect();
        assert!(!twins.is_empty(), "no near-collision names were generated");
        for t in twins {
            let src = c.fact(t.twin_of.as_ref().unwrap()).unwrap();
            let (a, b) = (
                t.subject.split(' ').next().unwrap(),
                src.subject.split(' ').next().unwrap(),
            );
            assert_ne!(a, b, "a twin must not be the same name");
            assert_eq!(
                a.chars().next(),
                b.chars().next(),
                "twins share an onset: {a} vs {b}"
            );
        }
    }
}
