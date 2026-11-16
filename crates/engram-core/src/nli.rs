//! NLI, the cortex's logic layer: a local cross-encoder labels a premise and
//! hypothesis entailment, neutral or contradiction ("we use Postgres" vs "we
//! use MySQL"), offline. Models don't validate: NLI nominates and orders,
//! never moves trust.
//!
//! Default: `sileod/deberta-v3-small-tasksource-nli` (own quantized ONNX
//! export, 172 MB, 512-token pairs), chosen on `eval/CONTRADICTIONS.md`.
//! MNLI-only models presuppose co-reference and call unrelated notes
//! contradictions; tasksource halves false alarms at the shipped gate. Laya
//! models ([`crate::laya`], via [`load_dir`]) are the alternatives in
//! `cortex::presets`.
//!
//! `ENGRAM_NLI_DIR` swaps the model: `model.onnx`, `tokenizer.json` and a
//! `config.json` whose `id2label` covers all three labels. Binary "zero-shot"
//! models can't express contradiction and won't load.

#[cfg(feature = "fastembed")]
use crate::sync::MutexExt;
use serde::{Deserialize, Serialize};

use crate::Result;

/// One three-way judgment over a (premise, hypothesis) pair. Probabilities
/// sum to ~1 (softmax over the model's logits).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct NliJudgment {
    pub entailment: f32,
    pub neutral: f32,
    pub contradiction: f32,
}

impl NliJudgment {
    pub fn label(&self) -> &'static str {
        if self.contradiction >= self.entailment && self.contradiction >= self.neutral {
            "contradiction"
        } else if self.entailment >= self.neutral {
            "entailment"
        } else {
            "neutral"
        }
    }
}

pub trait Nli: Send + Sync {
    /// Judge a batch of (premise, hypothesis) pairs.
    fn judge(&self, pairs: &[(String, String)]) -> Result<Vec<NliJudgment>>;

    /// Judge one pair in both directions and fold: contradiction is roughly
    /// symmetric (take the max), entailment is directional (keep forward =
    /// premise→hypothesis; the reverse ride-along is the `replaces` signal).
    fn judge_pair(&self, premise: &str, hypothesis: &str) -> Result<SymmetricJudgment> {
        let mut out = self.judge(&[
            (premise.to_string(), hypothesis.to_string()),
            (hypothesis.to_string(), premise.to_string()),
        ])?;
        let backward = out.pop().expect("two judgments for two pairs");
        let forward = out.pop().expect("two judgments for two pairs");
        Ok(SymmetricJudgment { forward, backward })
    }

    /// Which model this is — the model directory's name for an ONNX model.
    /// A verdict remembered by a sweep is only reusable under the model that
    /// made it; an empty id means "unknown", so remembered verdicts
    /// still hold until the model reports a different one.
    fn model_id(&self) -> String {
        String::new()
    }
}

/// Shared NLI runtime across stores — same `Arc` sharing as
/// the embedder and reranker.
impl<T: Nli + ?Sized> Nli for std::sync::Arc<T> {
    fn judge(&self, pairs: &[(String, String)]) -> Result<Vec<NliJudgment>> {
        (**self).judge(pairs)
    }

    fn model_id(&self) -> String {
        (**self).model_id()
    }
}

/// A directory's last path component — how a local ONNX model is named.
#[cfg_attr(not(feature = "fastembed"), allow(dead_code))]
pub(crate) fn dir_model_id(dir: &std::path::Path) -> String {
    dir.file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Both directions of one pair, for callers that need entailment asymmetry
/// (duplicate vs supersession) on top of symmetric contradiction.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SymmetricJudgment {
    pub forward: NliJudgment,
    pub backward: NliJudgment,
}

impl SymmetricJudgment {
    pub fn contradiction(&self) -> f32 {
        self.forward.contradiction.max(self.backward.contradiction)
    }

    /// Both directions entail — the two texts state the same thing.
    pub fn mutual_entailment(&self) -> f32 {
        self.forward.entailment.min(self.backward.entailment)
    }

    /// The queue hint stored on a suspect: label + the probability behind it.
    pub fn hint(&self) -> (&'static str, f32) {
        let contra = self.contradiction();
        let mutual = self.mutual_entailment();
        if contra >= 0.5 {
            ("contradiction", contra)
        } else if mutual >= 0.5 {
            ("entailment", mutual)
        } else {
            ("neutral", self.forward.neutral.min(1.0))
        }
    }
}

/// Deterministic stand-in for tests: `CONTRA` in both texts → contradiction;
/// one text containing the other → entailment; otherwise neutral.
pub struct FakeNli;

impl Nli for FakeNli {
    fn model_id(&self) -> String {
        "fake".into()
    }

    fn judge(&self, pairs: &[(String, String)]) -> Result<Vec<NliJudgment>> {
        Ok(pairs
            .iter()
            .map(|(p, h)| {
                let (pl, hl) = (p.to_lowercase(), h.to_lowercase());
                if pl.contains("contra") && hl.contains("contra") {
                    // Directional variant for tests: a "neg"-marked side
                    // judged as the HYPOTHESIS contradicts hardest — lets
                    // tests exercise the contradiction-carrier asymmetry.
                    let contradiction = if hl.contains("neg") && !pl.contains("neg") {
                        0.95
                    } else if pl.contains("neg") && !hl.contains("neg") {
                        0.7
                    } else {
                        0.9
                    };
                    NliJudgment {
                        entailment: 0.02,
                        neutral: 0.08,
                        contradiction,
                    }
                } else if pl.contains(&hl) || hl.contains(&pl) {
                    NliJudgment {
                        entailment: 0.9,
                        neutral: 0.08,
                        contradiction: 0.02,
                    }
                } else {
                    NliJudgment {
                        entailment: 0.05,
                        neutral: 0.9,
                        contradiction: 0.05,
                    }
                }
            })
            .collect())
    }
}

#[cfg(feature = "fastembed")]
mod fast {
    use std::path::Path;
    use std::sync::Mutex;

    use ndarray::Array;
    use ort::session::Session;
    use ort::value::Value;
    use tokenizers::Tokenizer;

    use super::*;

    /// Local ONNX NLI over the same `ort` runtime fastembed links. Loads only
    /// from a local directory — downloading is the CLI's job (curl, like
    /// self-update), so core stays free of HTTP.
    pub struct FastNli {
        tokenizer: Tokenizer,
        session: Mutex<Session>,
        need_token_type_ids: bool,
        /// logits index of each class, read from config.json's id2label.
        entail_idx: usize,
        neutral_idx: usize,
        contra_idx: usize,
        /// The model directory's name (see [`Nli::model_id`]).
        id: String,
    }

    impl FastNli {
        pub fn new() -> Result<Self> {
            let dir = nli_model_dir().ok_or_else(|| {
                crate::Error::Embedding("no home directory for the NLI model cache".into())
            })?;
            Self::from_dir(&dir)
        }

        pub fn from_dir(dir: &Path) -> Result<Self> {
            let path = |name: &str| dir.join(name);
            // Session policy lives in `crate::onnx` — arena off, no
            // memory-pattern pre-planner, threads left to ORT unless capped.
            // Allocation only: the logits this model produces are unchanged.
            // Each `with_*` hands its builder back inside the error type, so
            // the steps unwrap one at a time rather than chaining `and_then`.
            let mut builder = Session::builder().map_err(nli_err)?;
            builder = builder
                .with_execution_providers(crate::onnx::execution_providers())
                .map_err(nli_err)?;
            if let Some(t) = crate::onnx::intra_threads() {
                builder = builder.with_intra_threads(t).map_err(nli_err)?;
            }
            builder = builder.with_memory_pattern(false).map_err(nli_err)?;
            let session = builder
                .commit_from_file(path("model.onnx"))
                .map_err(nli_err)?;
            let need_token_type_ids = session
                .inputs()
                .iter()
                .any(|input| input.name() == "token_type_ids");

            let mut tokenizer = Tokenizer::from_file(path("tokenizer.json")).map_err(nli_err)?;
            tokenizer
                .with_truncation(Some(tokenizers::TruncationParams {
                    max_length: 512,
                    ..Default::default()
                }))
                .map_err(nli_err)?;
            tokenizer.with_padding(Some(tokenizers::PaddingParams::default()));

            let config: serde_json::Value = serde_json::from_slice(
                &std::fs::read(path("config.json"))
                    .map_err(|e| crate::Error::Embedding(format!("reading config.json: {e}")))?,
            )?;
            let idx_of = |label: &str| -> Result<usize> {
                config["id2label"]
                    .as_object()
                    .and_then(|m| {
                        m.iter()
                            .find(|(_, v)| {
                                v.as_str().is_some_and(|s| s.eq_ignore_ascii_case(label))
                            })
                            .and_then(|(k, _)| k.parse().ok())
                    })
                    .ok_or_else(|| {
                        crate::Error::Embedding(format!("config.json id2label lacks {label}"))
                    })
            };
            Ok(Self {
                entail_idx: idx_of("entailment")?,
                neutral_idx: idx_of("neutral")?,
                contra_idx: idx_of("contradiction")?,
                tokenizer,
                session: Mutex::new(session),
                need_token_type_ids,
                id: super::dir_model_id(dir),
            })
        }
    }

    impl Nli for FastNli {
        /// Judge every pair in batches of at most [`crate::onnx::batch`]. Pairs are
        /// scored independently and padding is masked, so chunking is exact.
        fn judge(&self, pairs: &[(String, String)]) -> Result<Vec<NliJudgment>> {
            let mut out = Vec::with_capacity(pairs.len());
            for chunk in pairs.chunks(crate::onnx::batch()) {
                out.extend(self.judge_batch(chunk)?);
            }
            Ok(out)
        }

        fn model_id(&self) -> String {
            self.id.clone()
        }
    }

    impl FastNli {
        fn judge_batch(&self, pairs: &[(String, String)]) -> Result<Vec<NliJudgment>> {
            if pairs.is_empty() {
                return Ok(Vec::new());
            }
            let inputs: Vec<tokenizers::EncodeInput> = pairs
                .iter()
                .map(|(p, h)| (p.as_str(), h.as_str()).into())
                .collect();
            let encodings = self.tokenizer.encode_batch(inputs, true).map_err(nli_err)?;
            let len = encodings
                .first()
                .map(tokenizers::Encoding::len)
                .ok_or_else(|| crate::Error::Embedding("tokenizer returned nothing".into()))?;
            let batch = encodings.len();

            let mut ids = Vec::with_capacity(batch * len);
            let mut mask = Vec::with_capacity(batch * len);
            let mut type_ids = Vec::with_capacity(batch * len);
            for e in &encodings {
                ids.extend(e.get_ids().iter().map(|&x| i64::from(x)));
                mask.extend(e.get_attention_mask().iter().map(|&x| i64::from(x)));
                type_ids.extend(e.get_type_ids().iter().map(|&x| i64::from(x)));
            }
            let shape = (batch, len);
            let mut session_inputs = ort::inputs![
                "input_ids" => Value::from_array(Array::from_shape_vec(shape, ids).map_err(nli_err)?).map_err(nli_err)?,
                "attention_mask" => Value::from_array(Array::from_shape_vec(shape, mask).map_err(nli_err)?).map_err(nli_err)?,
            ];
            if self.need_token_type_ids {
                session_inputs.push((
                    "token_type_ids".into(),
                    Value::from_array(Array::from_shape_vec(shape, type_ids).map_err(nli_err)?)
                        .map_err(nli_err)?
                        .into(),
                ));
            }

            // With the arena on, ask ORT to shrink it after the run. Gated, because a
            // shrink with no arena registered is an error.
            let run_opts = if crate::onnx::arena_enabled() {
                let mut o = ort::session::RunOptions::new().map_err(nli_err)?;
                o.add_config_entry("memory.enable_memory_arena_shrinkage", "cpu:0")
                    .map_err(nli_err)?;
                Some(o)
            } else {
                None
            };
            let mut session = self.session.locked();
            let outputs = match &run_opts {
                Some(o) => session.run_with_options(session_inputs, o),
                None => session.run(session_inputs),
            }
            .map_err(nli_err)?;
            let logits = outputs
                .get("logits")
                .ok_or_else(|| crate::Error::Embedding("model output lacks 'logits'".into()))?
                .try_extract_array::<f32>()
                .map_err(nli_err)?;

            let mut out = Vec::with_capacity(batch);
            for row in logits
                .to_shape((batch, logits.len() / batch))
                .map_err(nli_err)?
                .rows()
            {
                let max = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let exp: Vec<f32> = row.iter().map(|l| (l - max).exp()).collect();
                let sum: f32 = exp.iter().sum();
                out.push(NliJudgment {
                    entailment: exp[self.entail_idx] / sum,
                    neutral: exp[self.neutral_idx] / sum,
                    contradiction: exp[self.contra_idx] / sum,
                });
            }
            Ok(out)
        }
    }

    fn nli_err(e: impl std::fmt::Display) -> crate::Error {
        crate::Error::Embedding(format!("nli: {e}"))
    }
}

#[cfg(feature = "fastembed")]
pub use fast::FastNli;

/// Load whatever judge `dir` holds: a Laya typed-decision model (marked by
/// `rl_agent_config.json`, see [`crate::laya`]) or a three-label NLI export.
/// The engine only ever sees the [`Nli`] contract, so the swap is a directory.
#[cfg(feature = "fastembed")]
pub fn load_dir(dir: &std::path::Path) -> Result<Box<dyn Nli>> {
    if crate::laya::is_laya_dir(dir) {
        Ok(Box::new(crate::laya::LayaNli::from_dir(dir)?))
    } else {
        Ok(Box::new(FastNli::from_dir(dir)?))
    }
}

/// The three files FastNli loads. `model.onnx` is the repo's
/// `model_quantized.onnx` saved under the plain name.
pub const NLI_MODEL_FILES: [&str; 3] = ["model.onnx", "tokenizer.json", "config.json"];
/// Where the CLI downloads the model to and FastNli loads it from
/// (overridable via `ENGRAM_NLI_DIR`).
pub const NLI_MODEL_NAME: &str = "deberta-v3-small-tasksource-nli";

/// `ENGRAM_NLI_DIR`, else `~/.cache/engram/<NLI_MODEL_NAME>`. Pure path
/// logic, outside the `fastembed` gate so the HTTP crate builds alone.
pub fn nli_model_dir() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("ENGRAM_NLI_DIR") {
        return Some(std::path::PathBuf::from(dir));
    }
    crate::rag::home().map(|h| h.join(".cache/engram").join(NLI_MODEL_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Manual sanity check of the real runtime model against canonical NLI
    /// pairs plus one of our own title-shaped inputs. Run with:
    /// `cargo test -p engram-core -F fastembed real_nli -- --ignored --nocapture`
    #[test]
    #[ignore = "needs the downloaded NLI model in ~/.cache/engram"]
    #[cfg(feature = "fastembed")]
    fn real_nli_canonical_pairs() {
        let n = FastNli::new().expect("model downloaded");
        let pairs = [
            (
                "A man is playing a guitar on stage",
                "A man is playing an instrument",
            ),
            (
                "The cat is sleeping on the couch",
                "The cat is running in the yard",
            ),
            ("We deploy on Fridays", "The database uses WAL mode"),
            (
                "Timeline pane view shipped: a History section renders the replaces chain oldest-first",
                "Verified code refs shipped: drift sweep plus GET /drift and Review badges",
            ),
        ];
        for (p, h) in pairs {
            let j = n
                .judge(&[(p.to_string(), h.to_string())])
                .unwrap()
                .remove(0);
            println!(
                "{:13} e={:.2} n={:.2} c={:.2}  {p} || {h}",
                j.label(),
                j.entailment,
                j.neutral,
                j.contradiction
            );
        }
    }

    #[test]
    fn fake_nli_covers_the_three_labels() {
        let n = FakeNli;
        let sym = n
            .judge_pair("CONTRA: we use tabs", "CONTRA: we use spaces")
            .unwrap();
        assert_eq!(sym.hint().0, "contradiction");
        assert!(sym.contradiction() > 0.8);

        let dup = n
            .judge_pair("sessions live in cookies", "sessions live in cookies")
            .unwrap();
        assert_eq!(dup.hint().0, "entailment");
        assert!(dup.mutual_entailment() > 0.8);

        let unrelated = n
            .judge_pair("we ship on fridays", "the parser uses nom")
            .unwrap();
        assert_eq!(unrelated.hint().0, "neutral");
    }
}
