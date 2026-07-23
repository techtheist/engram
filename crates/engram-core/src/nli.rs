//! NLI — the logic layer of the local cortex (PLAN §7A). Given a premise and
//! a hypothesis, a small local cross-encoder answers: entailment, neutral, or
//! contradiction. This is what similarity can't do — telling "we use Postgres"
//! vs "we use MySQL" (contradiction) apart from "we use Postgres for analytics
//! only" (refinement) — and it runs offline, token-free.
//!
//! Governing principle (PLAN §7A): **models don't validate.** NLI output
//! nominates — orders the suspects queue, suggests verdicts, annotates claims
//! — and never moves a trust field. Judgment stays with the user or the
//! assistant.
//!
//! Runtime model: `Xenova/nli-deberta-v3-small` (the transformers.js-standard
//! ONNX export of `cross-encoder/nli-deberta-v3-small`, quantized ~34 MB,
//! 512-token pairs). Chosen for maximum compatibility: plain BERT-shaped
//! inputs, `tokenizer.json` included, label map read from `config.json` at
//! load. `finecat-nli-m` stays the eval-side benchmark (`scripts/nli-eval.py`)
//! until it ships ONNX.

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
}

/// Shared NLI runtime across stores (PLAN §7C hub) — same `Arc` sharing as
/// the embedder and reranker.
impl<T: Nli + ?Sized> Nli for std::sync::Arc<T> {
    fn judge(&self, pairs: &[(String, String)]) -> Result<Vec<NliJudgment>> {
        (**self).judge(pairs)
    }
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
            let session = Session::builder()
                .and_then(|mut b| b.commit_from_file(path("model.onnx")))
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
            })
        }
    }

    impl Nli for FastNli {
        fn judge(&self, pairs: &[(String, String)]) -> Result<Vec<NliJudgment>> {
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
                .map(|e| e.len())
                .ok_or_else(|| crate::Error::Embedding("tokenizer returned nothing".into()))?;
            let batch = encodings.len();

            let mut ids = Vec::with_capacity(batch * len);
            let mut mask = Vec::with_capacity(batch * len);
            let mut type_ids = Vec::with_capacity(batch * len);
            for e in &encodings {
                ids.extend(e.get_ids().iter().map(|x| *x as i64));
                mask.extend(e.get_attention_mask().iter().map(|x| *x as i64));
                type_ids.extend(e.get_type_ids().iter().map(|x| *x as i64));
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

            let mut session = self.session.lock().expect("nli session mutex");
            let outputs = session.run(session_inputs).map_err(nli_err)?;
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
                let max = row.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
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

/// The three files FastNli loads. `model.onnx` is Xenova's
/// `model_quantized.onnx` saved under the plain name.
pub const NLI_MODEL_FILES: [&str; 3] = ["model.onnx", "tokenizer.json", "config.json"];
/// Where the CLI downloads the model to and FastNli loads it from
/// (overridable via `ENGRAM_NLI_DIR`).
pub const NLI_MODEL_NAME: &str = "nli-deberta-v3-small";

/// `ENGRAM_NLI_DIR`, else `~/.cache/engram/nli-deberta-v3-small`. Pure path
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
