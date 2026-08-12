//! The LongMemEval adapter — the first corpus this harness runs that we did
//! not generate.
//!
//! [LongMemEval](https://github.com/xiaowu0162/LongMemEval) (Wu et al., MIT
//! license) is 500 questions, each over its own multi-session chat history,
//! with the evidence sessions labelled and ~6% of the questions deliberately
//! unanswerable (`_abs`). Those labels are what make it usable here: grading
//! is *retrieval* — did a note from a labelled answer session come back, at
//! what cost, under what verdict — full population, deterministic, no LLM
//! judge anywhere. The abstention questions land straight on the mechanism
//! this project measures that competitors do not: the calibrated "likely not
//! in memory" line.
//!
//! The dataset is never checked into the repo. It is fetched on demand into
//! `eval/data/` (gitignored), verified against a pinned SHA-256, and reused
//! from cache forever after; the repo carries only this loader, the digests,
//! and the provenance note above.
//!
//! Honesty note on ingestion: Engram deliberately ships no extractor — the
//! agent writes typed notes. This adapter's arm is therefore **as-is**: every
//! chat turn becomes one note, verbatim. That is the unflattering register
//! (chat filler and all) and it is labelled as such; an agent-extracted arm
//! would be a separate, disclosed artifact.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use engram_core::config::GraphConfig;
use engram_core::{Durability, Embedder, Engine, NewNode, NodeType, Reranker, SqliteStore};

use crate::arms::tokens;
use crate::run::{Config, DEFAULT_CURATED_BUDGET};

const HF_BASE: &str = "https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned/resolve/main";

/// (cli key, file name, pinned sha256). The `m` variant (2.7 GB, ~500
/// sessions per question) is deliberately absent — nothing it answers that
/// `s` does not is worth the download.
const DATASETS: [(&str, &str, &str); 2] = [
    (
        "s",
        "longmemeval_s_cleaned.json",
        "d6f21ea9d60a0d56f34a05b609c79c88a451d2ae03597821ea3d5a9678c3a442",
    ),
    (
        "oracle",
        "longmemeval_oracle.json",
        "821a2034d219ab45846873dd14c14f12cfe7776e73527a483f9dac095d38620c",
    ),
];

#[derive(Debug, Clone, Deserialize)]
pub struct LmeTurn {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub has_answer: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LmeQuestion {
    pub question_id: String,
    #[serde(default)]
    pub question_type: String,
    pub question: String,
    /// Kept for inspection only — grading never reads it (grading is
    /// retrieval against `answer_session_ids`, not answer text).
    #[serde(default)]
    pub answer: serde_json::Value,
    #[serde(default)]
    pub question_date: String,
    pub haystack_session_ids: Vec<String>,
    #[serde(default)]
    pub haystack_dates: Vec<String>,
    pub haystack_sessions: Vec<Vec<LmeTurn>>,
    #[serde(default)]
    pub answer_session_ids: Vec<String>,
}

impl LmeQuestion {
    /// LongMemEval marks its deliberately-unanswerable questions by suffix.
    pub fn is_abstention(&self) -> bool {
        self.question_id.ends_with("_abs")
    }
}

/// The chat ontology, defined as data — the whole point of per-graph
/// GraphConfig is that the same engine fits a register it was not written
/// for without a line of engine code changing.
///
/// LongMemEval is personal conversational memory, not software decisions, so
/// the default eight-type ontology misfits it twice over: everything lands as
/// `Insight`, and the type layer contributes nothing. Two types replace it,
/// carrying the ONE distinction the as-is ingester can make honestly with no
/// classifier and no model — who spoke:
///
/// * `statement` — a user turn: first-party facts about the user's life.
///   Carries a small rank prior, the same tilt a `Principle` gets in the
///   shipped set: when a statement and a restating reply score alike, the
///   first-party source should win the tie.
/// * `reply` — an assistant turn: mostly restatement and filler, kept because
///   evidence turns can sit on either side of the dialogue. No prior, muted.
///
/// Verbs, policy, and brief shape stay stock — this arm writes no edges, and
/// the exercise is the type layer, not a tuning pass.
pub fn chat_config(mut cfg: GraphConfig) -> GraphConfig {
    let template = cfg.ontology.types[0].clone();
    let mut statement = template.clone();
    statement.name = "statement".into();
    statement.hue = 210;
    statement.thought = "Something the user said about their own life".into();
    statement.durability = Durability::Stable;
    statement.roles.worklist = false;
    statement.roles.anchor = false;
    statement.roles.rank_prior = 0.05;
    statement.roles.highlight = true;
    statement.roles.versioned = false;

    let mut reply = template;
    reply.name = "reply".into();
    reply.hue = 30;
    reply.thought = "What the assistant said back — restatement, not source".into();
    reply.durability = Durability::Stable;
    reply.roles.worklist = false;
    reply.roles.anchor = false;
    reply.roles.rank_prior = 0.0;
    reply.roles.highlight = false;
    reply.roles.versioned = false;

    cfg.ontology.preset = "chat".into();
    cfg.ontology.types = vec![statement, reply];
    cfg
}

/// Unix seconds from a LongMemEval date stamp ("2023/05/20 (Sat) 02:21").
/// Session dates become `created_at` — the knowledge's original date, which
/// is exactly what that field is documented for — so recency reads the real
/// conversation timeline instead of a flat ingestion instant.
fn unix_from_lme_date(s: &str) -> Option<i64> {
    let mut parts = s.split_whitespace();
    let date = parts.next()?;
    let mut it = date.split('/');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    let (hh, mm) = parts
        .next_back()
        .and_then(|t| t.split_once(':'))
        .and_then(|(h, mn)| Some((h.parse::<i64>().ok()?, mn.parse::<i64>().ok()?)))
        .unwrap_or((0, 0));
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm), then clock time.
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3_600 + mm * 60)
}

fn sha256_hex(path: &std::path::Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data")
}

/// Return the local path of a dataset, downloading and digest-checking it on
/// first use. A cached file is re-verified every time — a truncated download
/// must never quietly grade as a small corpus.
pub fn fetch(key: &str) -> anyhow::Result<PathBuf> {
    let (_, file, sha) = DATASETS
        .iter()
        .find(|(k, _, _)| *k == key)
        .ok_or_else(|| anyhow::anyhow!("unknown LongMemEval variant {key:?} (s | oracle)"))?;
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(file);

    if !path.exists() {
        let url = format!("{HF_BASE}/{file}");
        eprintln!("  fetching {url} -> {} (one-time)", path.display());
        let part = dir.join(format!("{file}.part"));
        let status = std::process::Command::new("curl")
            .args(["-L", "--fail", "--progress-bar", "-o"])
            .arg(&part)
            .arg(&url)
            .status()
            .map_err(|e| {
                anyhow::anyhow!(
                    "could not run curl ({e}) — install curl or place {file} in eval/data/ by hand"
                )
            })?;
        anyhow::ensure!(status.success(), "download failed: {url}");
        std::fs::rename(&part, &path)?;
    }

    let got = sha256_hex(&path)?;
    anyhow::ensure!(
        got == *sha,
        "digest mismatch for {}: got {got}, pinned {sha} — delete the file and re-run",
        path.display()
    );
    Ok(path)
}

pub fn load(path: &std::path::Path) -> anyhow::Result<Vec<LmeQuestion>> {
    let f = std::fs::File::open(path)?;
    Ok(serde_json::from_reader(std::io::BufReader::new(f))?)
}

/// Rank (0-based) of the first delivered note that came from a labelled
/// answer session.
fn evidence_rank(delivered_sids: &[&str], answers: &[String]) -> Option<usize> {
    delivered_sids
        .iter()
        .position(|sid| answers.iter().any(|a| a == sid))
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LmeArmScore {
    pub queries: usize,
    pub recall_at_1: f64,
    pub recall_at_5: f64,
    pub mrr: f64,
    pub tokens_mean: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LmeTypeScore {
    pub question_type: String,
    pub score: LmeArmScore,
}

#[derive(Debug, Clone, Serialize)]
pub struct LmeNamedScore {
    pub arm: String,
    pub score: LmeArmScore,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LmeAbstention {
    /// `_abs` questions run.
    pub questions: usize,
    /// Delivered nothing — the `none` verdict.
    pub empty: usize,
    /// Answered under the calibrated "likely not in memory" line — honest.
    pub warned: usize,
    /// Answered at or above the line — the real false positives.
    pub unwarned: usize,
    /// Stores where auto-tune actually moved the line (small stores refuse).
    pub tuned_stores: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct LmeReport {
    pub dataset: String,
    pub file: String,
    pub questions_total: usize,
    pub questions_run: usize,
    /// True when `--lme-limit` dropped part of the population. A capped run
    /// is a smoke test, not a result — the cap is printed, never silent.
    pub capped: bool,
    pub embedder: String,
    pub reranker: String,
    pub embeddings_are_fake: bool,
    /// Which per-graph ontology the stores ran under: "chat" (fitted, the
    /// default) or "default" (the stock software set).
    pub ontology: String,
    pub limit: usize,
    pub notes_mean: f64,
    /// Every arm over the same haystacks: engram, rag (pure vectors), grep
    /// (keyword search over the flat turns), curated-file (a blind 3k-token
    /// selection — chat turns carry no types, so the human heuristic degrades
    /// to a stated hash order), whole-file (the entire haystack in context —
    /// the "just use a 128k window" answer, priced).
    pub arms: Vec<LmeNamedScore>,
    pub by_type: Vec<LmeTypeScore>,
    pub abstention: LmeAbstention,
}

struct Tally {
    queries: usize,
    hit1: usize,
    hit5: usize,
    mrr: f64,
    tokens: usize,
}

impl Tally {
    fn new() -> Self {
        Self {
            queries: 0,
            hit1: 0,
            hit5: 0,
            mrr: 0.0,
            tokens: 0,
        }
    }
    fn add(&mut self, rank: Option<usize>, tokens: usize) {
        self.queries += 1;
        self.tokens += tokens;
        if let Some(r) = rank {
            if r == 0 {
                self.hit1 += 1;
            }
            if r < 5 {
                self.hit5 += 1;
            }
            self.mrr += 1.0 / (r + 1) as f64;
        }
    }
    fn score(&self) -> LmeArmScore {
        let n = self.queries.max(1) as f64;
        LmeArmScore {
            queries: self.queries,
            recall_at_1: self.hit1 as f64 / n,
            recall_at_5: self.hit5 as f64 / n,
            mrr: self.mrr / n,
            tokens_mean: self.tokens as f64 / n,
        }
    }
}

/// Run the adapter: one in-memory store per question (each question owns its
/// haystack), the shipped cortex shared across all of them. `ontology` is
/// "chat" (the fitted two-type config, the default) or "default" (the stock
/// software ontology, every turn an `Insight`) — the delta between the two
/// runs is what the type layer buys on a register it was configured for.
pub fn run(
    cfg: &Config,
    dataset: &str,
    cap: Option<usize>,
    ontology: &str,
    embedder_kind: &str,
    workers: Option<usize>,
) -> anyhow::Result<LmeReport> {
    anyhow::ensure!(
        matches!(ontology, "chat" | "default"),
        "unknown --lme-ontology {ontology:?} (chat | default)"
    );
    anyhow::ensure!(
        matches!(embedder_kind, "fastembed" | "ollama"),
        "unknown --lme-embedder {embedder_kind:?} (fastembed | ollama)"
    );
    let path = fetch(dataset)?;
    let all = load(&path)?;
    let total = all.len();
    let run_n = cap.map(|n| n.min(total)).unwrap_or(total);
    if run_n < total {
        eprintln!("  ! --lme-limit kept {run_n} of {total} questions — a smoke run, not a result");
    }

    // The GPU swap changes the runtime, not the model: same bge-small
    // weights, CLS pooling, normalised output — served by Ollama on Metal
    // instead of onnxruntime on CPU (see `crate::ollama`).
    let (embed_box, embedder_name): (Box<dyn Embedder>, String) = if embedder_kind == "ollama" {
        let e = crate::ollama::OllamaEmbedder::new()?;
        (
            Box::new(e),
            "bge-small-en-v1.5 (ollama gpu, gguf f16)".to_string(),
        )
    } else {
        crate::run::embedder(cfg.embed_model.as_deref())
    };
    let embed: Arc<dyn Embedder> = Arc::from(embed_box);
    let (rr, reranker_name) = if cfg.no_rerank {
        (None, "disabled (--no-rerank)".to_string())
    } else {
        crate::run::reranker()
    };
    let rerank: Option<Arc<dyn Reranker>> = rr.map(Arc::from);
    let dim = embed.embed_one("dimension probe")?.len();

    let mut engram = Tally::new();
    let mut rag = Tally::new();
    let mut grep = Tally::new();
    let mut curated = Tally::new();
    let mut whole = Tally::new();
    let mut by_type: HashMap<String, Tally> = HashMap::new();
    let mut abstention = LmeAbstention::default();
    let mut notes_total = 0usize;
    let started = std::time::Instant::now();

    // Questions are independent worlds — one store, one grading each — so
    // they parallelise trivially. Against the GPU embedder the win is real:
    // the server pipelines concurrent requests, while sequential batch-1
    // calls leave it half idle. Against the Mutex-serialised fastembed path
    // extra workers buy little, so the default stays 1 there.
    let workers = workers
        .unwrap_or(if embedder_kind == "ollama" { 8 } else { 1 })
        .max(1);
    if workers > 1 {
        eprintln!("  {workers} parallel workers");
    }
    let qs: Vec<&LmeQuestion> = all.iter().take(run_n).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<anyhow::Result<QOutcome>>>> =
        (0..qs.len()).map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let qi = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(&q) = qs.get(qi) else { break };
                    let out = run_question(cfg, ontology, &embed, &rerank, dim, q);
                    *slots[qi].lock().unwrap() = Some(out);
                    let d = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if d.is_multiple_of(10) {
                        eprintln!(
                            "  {d}/{} questions ({:.0}s elapsed)",
                            qs.len(),
                            started.elapsed().as_secs_f64()
                        );
                    }
                }
            });
        }
    });

    // Aggregation stays in question order — the report is identical to the
    // sequential run's, whatever order the workers finished in.
    for slot in slots {
        let out = slot
            .into_inner()
            .unwrap()
            .expect("every claimed slot is filled")?;
        notes_total += out.notes;
        match out.verdict {
            QVerdict::Abstention(a) => {
                abstention.questions += 1;
                if a.empty {
                    abstention.empty += 1;
                } else {
                    if a.tuned {
                        abstention.tuned_stores += 1;
                    }
                    if a.unwarned {
                        abstention.unwarned += 1;
                    } else {
                        abstention.warned += 1;
                    }
                }
            }
            QVerdict::Graded(g) => {
                engram.add(g.engram.rank, g.engram.cost);
                by_type
                    .entry(out.question_type.clone())
                    .or_insert_with(Tally::new)
                    .add(g.engram.rank, g.engram.cost);
                rag.add(g.rag.rank, g.rag.cost);
                grep.add(g.grep.rank, g.grep.cost);
                curated.add(g.curated.rank, g.curated.cost);
                whole.add(g.whole.rank, g.whole.cost);
            }
        }
    }

    let mut by_type: Vec<LmeTypeScore> = by_type
        .into_iter()
        .map(|(question_type, t)| LmeTypeScore {
            question_type,
            score: t.score(),
        })
        .collect();
    by_type.sort_by(|a, b| a.question_type.cmp(&b.question_type));

    Ok(LmeReport {
        dataset: dataset.to_string(),
        file: path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default(),
        questions_total: total,
        questions_run: run_n,
        capped: run_n < total,
        embeddings_are_fake: embedder_name.contains("(fake)"),
        embedder: embedder_name,
        reranker: reranker_name,
        ontology: ontology.to_string(),
        limit: cfg.limit,
        notes_mean: notes_total as f64 / run_n.max(1) as f64,
        arms: [
            ("engram", engram),
            ("rag", rag),
            ("grep", grep),
            ("curated-file", curated),
            ("whole-file", whole),
        ]
        .into_iter()
        .map(|(name, t)| LmeNamedScore {
            arm: name.to_string(),
            score: t.score(),
        })
        .collect(),
        by_type,
        abstention,
    })
}

/// One arm's grade on one question: where the evidence landed, what the
/// delivery billed.
struct ArmGrade {
    rank: Option<usize>,
    cost: usize,
}

struct ArmsGrade {
    engram: ArmGrade,
    rag: ArmGrade,
    grep: ArmGrade,
    curated: ArmGrade,
    whole: ArmGrade,
}

struct AbsGrade {
    empty: bool,
    unwarned: bool,
    tuned: bool,
}

enum QVerdict {
    Abstention(AbsGrade),
    Graded(ArmsGrade),
}

/// Everything one question contributes to the report — computed on a worker
/// thread, aggregated in question order on the main one.
struct QOutcome {
    question_type: String,
    notes: usize,
    verdict: QVerdict,
}

/// One question, one world: build the store, ingest the haystack, grade
/// every arm. Self-contained so questions can run on parallel workers.
fn run_question(
    cfg: &Config,
    ontology: &str,
    embed: &Arc<dyn Embedder>,
    rerank: &Option<Arc<dyn Reranker>>,
    dim: usize,
    q: &LmeQuestion,
) -> anyhow::Result<QOutcome> {
    let mut notes = 0usize;
    // One store per question: the haystack is this question's world.
    let store = SqliteStore::open_in_memory()?;
    {
        use engram_core::Store as _;
        store.reset_vectors(dim)?;
    }
    let mut engine = Engine::new(store, Box::new(embed.clone()));
    if let Some(r) = rerank {
        engine.set_reranker(Box::new(r.clone()));
    }
    // The fitted ontology goes in BEFORE ingestion — type existence is
    // checked at the engine's write boundary, exactly as in the product.
    if ontology == "chat" {
        engine.set_graph_config(&chat_config(engine.graph_config()))?;
    }
    let (statement, reply) = (NodeType::parse("statement")?, NodeType::parse("reply")?);

    // As-is ingestion: one note per turn, verbatim, both roles — the
    // labelled evidence turns can sit on either side of the dialogue.
    // Under the chat ontology the turn's role picks its type; the note is
    // stamped with the session's real date either way. The flat copy of
    // the same turns feeds the file-shaped baselines.
    let mut sid_of: HashMap<String, String> = HashMap::new();
    let mut flat_turns: Vec<(String, String)> = Vec::new();
    for (i, (sid, session)) in q
        .haystack_session_ids
        .iter()
        .zip(&q.haystack_sessions)
        .enumerate()
    {
        let session_date = q.haystack_dates.get(i).and_then(|d| unix_from_lme_date(d));
        for turn in session {
            let content = turn.content.trim();
            if content.is_empty() {
                continue;
            }
            let node_type = if ontology == "chat" {
                if turn.role == "user" {
                    statement.clone()
                } else {
                    reply.clone()
                }
            } else {
                NodeType::Insight
            };
            let title: String = content.chars().take(120).collect();
            let node = engine.add_node(NewNode {
                node_type,
                title: title.replace('\n', " "),
                body: Some(content.to_string()),
                created_at: session_date,
                durability: engram_core::Durability::Stable,
                source: engram_core::Source::Claude,
                session_id: Some(sid.clone()),
                status: None,
                code_refs: vec![],
                tags: vec![],
                version: None,
                props: None,
            })?;
            sid_of.insert(node.id, sid.clone());
            flat_turns.push((sid.clone(), content.to_string()));
            notes += 1;
        }
    }

    // The engram arm, scored exactly like `EngramArm::retrieve` bills it.
    let hits = engine
        .search(&q.question, &[], cfg.limit)
        .unwrap_or_default();
    let delivered_sids: Vec<&str> = hits
        .iter()
        .filter_map(|h| sid_of.get(&h.id).map(String::as_str))
        .collect();
    let cost: usize = hits
        .iter()
        .map(|h| tokens(&h.title) + tokens(&h.snippet))
        .sum();

    if q.is_abstention() {
        let grade = if hits.is_empty() {
            AbsGrade {
                empty: true,
                unwarned: false,
                tuned: false,
            }
        } else {
            // The line the product would run: auto-tune's phantom-probe
            // dial, converged (moves are damped), on this store's own
            // vocabulary. Only fitted where it is read — here.
            let mut tuned = false;
            for _ in 0..16 {
                match engine.auto_tune()? {
                    Some(_) => tuned = true,
                    None => break,
                }
            }
            let weak_line = engine.graph_config().policy.weak_evidence_top;
            let top = hits.first().map(|h| h.score).unwrap_or(0.0);
            AbsGrade {
                empty: false,
                unwarned: top >= weak_line,
                tuned,
            }
        };
        return Ok(QOutcome {
            question_type: q.question_type.clone(),
            notes,
            verdict: QVerdict::Abstention(grade),
        });
    }

    let engram_grade = ArmGrade {
        rank: evidence_rank(&delivered_sids, &q.answer_session_ids),
        cost,
    };

    // The pure-vector baseline on the same store and embeddings: nearest
    // vectors, whole notes, no keyword channel, no reranker, no graph.
    let store = engine.store();
    let qv = embed.embed_one(&q.question)?;
    let vec_hits = store.search_vec(&qv, cfg.limit).unwrap_or_default();
    let mut rag_sids = Vec::new();
    let mut rag_cost = 0usize;
    for (id, _) in &vec_hits {
        if let Ok(Some(node)) = store.get_node(id) {
            rag_cost += tokens(&node.title) + tokens(node.body.as_deref().unwrap_or(""));
        }
        if let Some(sid) = sid_of.get(id) {
            rag_sids.push(sid.as_str());
        }
    }
    let rag_grade = ArmGrade {
        rank: evidence_rank(&rag_sids, &q.answer_session_ids),
        cost: rag_cost,
    };

    // Keyword search over the flat turns — most-matched-terms first,
    // original order breaking ties, whole turns delivered: the same grep
    // baseline the offline suite carries, on chat instead of notes.
    let query_terms = crate::arms::terms(&q.question);
    let mut scored: Vec<(usize, usize)> = flat_turns
        .iter()
        .enumerate()
        .filter_map(|(i, (_, text))| {
            let hay = text.to_lowercase();
            let hits = query_terms.iter().filter(|t| hay.contains(*t)).count();
            (hits > 0).then_some((hits, i))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.truncate(cfg.limit);
    let grep_sids: Vec<&str> = scored
        .iter()
        .map(|(_, i)| flat_turns[*i].0.as_str())
        .collect();
    let grep_cost: usize = scored.iter().map(|(_, i)| tokens(&flat_turns[*i].1)).sum();
    let grep_grade = ArmGrade {
        rank: evidence_rank(&grep_sids, &q.answer_session_ids),
        cost: grep_cost,
    };

    // The hand-maintained file, chat edition. Chat turns carry no types,
    // so the offline suite's "durable types first" heuristic degrades to
    // its tie-break alone: a stated, question-blind hash order, entries
    // trimmed to 200 chars, filled to the same 3k-token budget. A dump —
    // a kept evidence turn counts as delivered, everything else is
    // unanswerable from the file. That ceiling is the measurement.
    let mut order: Vec<usize> = (0..flat_turns.len()).collect();
    order.sort_by_key(|i| crate::arms::scramble(&format!("t{i:05}")));
    let mut kept_sids: Vec<&str> = Vec::new();
    let mut curated_cost = 0usize;
    for i in order {
        let entry: String = flat_turns[i].1.chars().take(200).collect();
        let entry_cost = tokens(&entry);
        if curated_cost + entry_cost > DEFAULT_CURATED_BUDGET {
            continue;
        }
        curated_cost += entry_cost;
        kept_sids.push(flat_turns[i].0.as_str());
    }
    let curated_hit = kept_sids
        .iter()
        .any(|sid| q.answer_session_ids.iter().any(|a| a == sid));
    let curated_grade = ArmGrade {
        rank: curated_hit.then_some(0),
        cost: curated_cost,
    };

    // The whole haystack in context — LongMemEval-S is sized so this
    // needs a 128k window. It always contains the evidence; what it
    // costs to always contain the evidence is the row.
    let whole_cost: usize = flat_turns.iter().map(|(_, t)| tokens(t)).sum();

    Ok(QOutcome {
        question_type: q.question_type.clone(),
        notes,
        verdict: QVerdict::Graded(ArmsGrade {
            engram: engram_grade,
            rag: rag_grade,
            grep: grep_grade,
            curated: curated_grade,
            whole: ArmGrade {
                rank: Some(0),
                cost: whole_cost,
            },
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_entry_parses_from_the_documented_shape() {
        let json = r#"{
            "question_id": "gpt4_2655b836_abs",
            "question_type": "single-session-user",
            "question": "How many miles did I run last week?",
            "answer": "18 miles",
            "question_date": "2023/05/30 (Tue) 23:59",
            "haystack_session_ids": ["answer_2655b836", "s1"],
            "haystack_dates": ["2023/05/20 (Sat) 02:21", "2023/05/21 (Sun) 03:00"],
            "haystack_sessions": [
                [{"role": "user", "content": "I ran 18 miles.", "has_answer": true},
                 {"role": "assistant", "content": "Nice!"}],
                [{"role": "user", "content": "unrelated"}]
            ],
            "answer_session_ids": ["answer_2655b836"]
        }"#;
        let q: LmeQuestion = serde_json::from_str(json).unwrap();
        assert!(q.is_abstention());
        assert_eq!(q.haystack_sessions.len(), 2);
        assert_eq!(q.haystack_sessions[0][0].has_answer, Some(true));
        assert_eq!(q.answer_session_ids, ["answer_2655b836"]);
    }

    #[test]
    fn grading_is_by_evidence_session_not_by_text() {
        let answers = vec!["s2".to_string()];
        assert_eq!(evidence_rank(&["s9", "s2", "s2"], &answers), Some(1));
        assert_eq!(evidence_rank(&["s9", "s3"], &answers), None);
        assert_eq!(evidence_rank(&[], &answers), None);
    }

    #[test]
    fn the_chat_ontology_validates_and_the_engine_accepts_its_types() {
        use engram_core::{FakeEmbedder, Source};

        let store = SqliteStore::open_in_memory().unwrap();
        let engine = Engine::new(store, Box::new(FakeEmbedder::default()));
        let cfg = chat_config(engine.graph_config());
        assert_eq!(cfg.ontology.preset, "chat");
        assert_eq!(
            cfg.ontology.types.len(),
            2,
            "the whole point: types are data"
        );
        cfg.validate()
            .expect("the chat ontology must be a legal config");
        engine.set_graph_config(&cfg).unwrap();

        // The write boundary accepts the fitted types and refuses the stock
        // one it just replaced — existence is config-driven, as shipped.
        let note = |t: &str| NewNode {
            node_type: NodeType::parse(t).unwrap(),
            title: "I ran 18 miles last week".into(),
            body: None,
            created_at: unix_from_lme_date("2023/05/20 (Sat) 02:21"),
            durability: Durability::Stable,
            source: Source::Claude,
            session_id: None,
            status: None,
            code_refs: vec![],
            tags: vec![],
            version: None,
            props: None,
        };
        let n = engine.add_node(note("statement")).unwrap();
        assert_eq!(n.node_type.as_str(), "statement");
        assert!(n.created_at < 1_700_000_000, "the session date stuck");
        engine.add_node(note("reply")).unwrap();
        assert!(
            engine.add_node(note("Insight")).is_err(),
            "the replaced stock type must no longer exist in this graph"
        );
    }

    #[test]
    fn lme_dates_parse_to_unix_seconds() {
        // 2023-05-20 02:21 UTC.
        assert_eq!(
            unix_from_lme_date("2023/05/20 (Sat) 02:21"),
            Some(1_684_549_260)
        );
        // The date alone still parses; garbage does not.
        assert_eq!(unix_from_lme_date("1970/01/01 (Thu) 00:00"), Some(0));
        assert_eq!(unix_from_lme_date("2023/13/40 (??) 99:99"), None);
        assert_eq!(unix_from_lme_date("not a date"), None);
    }

    #[test]
    fn the_pinned_digests_are_wellformed() {
        for (key, file, sha) in DATASETS {
            assert!(!key.is_empty() && file.ends_with(".json"));
            assert_eq!(sha.len(), 64, "{file}: a sha256 is 64 hex chars");
            assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
}
