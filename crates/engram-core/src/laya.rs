//! Laya — typed decisions in one encoder pass (0.9.9 candidate for the logic
//! layer; github.com/NandhaKishorM/laya, Apache-2.0).
//!
//! Laya is not an NLI cross-encoder. It is an encoder plus a small decision
//! head that answers *typed questions* about a *state*: the sequence is
//! `[CLS] <type> question: <instructions> [SEP] [MASK] opt₀ [MASK] opt₁ …
//! [SEP] <state> [SEP]`, one logit is read at each `[MASK]` marker, and the
//! softmax over them — divided by a temperature fitted per (question type,
//! option count) — is the answer distribution. The prompt builder here is a
//! line-for-line port of `laya.common.build_sequence`; the parity test
//! against the Python reference is what keeps it honest.
//!
//! As an [`Nli`], [`LayaNli`] asks the question Laya's own XNLI benchmark
//! asks — the state `{"premise": …, "hypothesis": …}` and a three-way
//! `choice` keyed entailment / neutral / contradiction — so the rest of the
//! engine (and the model hot-swap) sees an ordinary three-way judge. The
//! typed interface ([`LayaModel::decide`]) stays public so the eval harness
//! can put graph context into the state.
//!
//! Directory contract (what `rl_agent_config.json` marks as a Laya model):
//! `model.onnx` (+ its external data, if any), `tokenizer.json` (+
//! `tokenizer_config.json` naming the special tokens), `rl_agent_config.json` (`max_len`, `head_max_len`, `temperature`,
//! `temperature_by_options`). The ONNX graph takes `input_ids`,
//! `attention_mask`, `marker_pos`, `marker_mask` (bool), `qtype` and
//! returns `logits` [batch, markers].

// The runtime lives behind `fastembed`; the prompt/temperature helpers it
// uses are pure and unit-tested without it.
#![cfg_attr(not(feature = "fastembed"), allow(dead_code))]

#[cfg(feature = "fastembed")]
use crate::sync::MutexExt;
use std::path::Path;

use serde::Deserialize;

/// The three question types; the discriminant is the model's `qtype` input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QType {
    Choice = 0,
    Score = 1,
    Noul = 2,
}

impl QType {
    fn name(self) -> &'static str {
        match self {
            QType::Choice => "choice",
            QType::Score => "score",
            QType::Noul => "noul",
        }
    }
}

/// One typed question with its options already rendered as the model reads
/// them (`"contradiction: the premise implies …"` for a described choice,
/// `"level 0: …"` for a score level, `"false: …"`/`"true: …"` for noul).
#[derive(Debug, Clone)]
pub struct Question {
    pub qtype: QType,
    pub instructions: String,
    pub options: Vec<String>,
}

impl Question {
    /// A `choice` over `(key, description)` pairs, rendered `key: description`.
    pub fn choice(instructions: &str, options: &[(&str, &str)]) -> Self {
        Question {
            qtype: QType::Choice,
            instructions: instructions.to_string(),
            options: options
                .iter()
                .map(|(k, v)| {
                    if v.is_empty() {
                        k.to_string()
                    } else {
                        format!("{k}: {v}")
                    }
                })
                .collect(),
        }
    }
}

/// The NLI question Laya's own XNLI benchmark asks; option order is the
/// label order [`LayaNli`] maps back.
pub fn nli_question() -> Question {
    Question::choice(
        "What is the relationship between `premise` and `hypothesis`?",
        &[
            ("entailment", "the premise implies the hypothesis is true"),
            (
                "neutral",
                "the premise neither implies nor contradicts the hypothesis",
            ),
            (
                "contradiction",
                "the premise implies the hypothesis is false",
            ),
        ],
    )
}

/// A JSON value the state serializer understands — enough for the states
/// engram builds (strings and lists of strings, in insertion order).
#[derive(Debug, Clone)]
pub enum StateValue {
    Text(String),
    List(Vec<String>),
}

/// Serialize an object the way Python's `json.dumps(obj, ensure_ascii=False)`
/// does — `", "` and `": "` separators, keys in the given order — because the
/// model was trained on states Python serialized, and a compact `{"a":"b"}`
/// is a different token sequence.
pub fn state_json(fields: &[(&str, StateValue)]) -> String {
    let s = |v: &str| serde_json::to_string(v).expect("strings serialize");
    let body: Vec<String> = fields
        .iter()
        .map(|(k, v)| {
            let v = match v {
                StateValue::Text(t) => s(t),
                StateValue::List(items) => format!(
                    "[{}]",
                    items.iter().map(|i| s(i)).collect::<Vec<_>>().join(", ")
                ),
            };
            format!("{}: {v}", s(k))
        })
        .collect();
    format!("{{{}}}", body.join(", "))
}

#[derive(Debug, Clone, Deserialize)]
pub struct LayaConfig {
    #[serde(default = "default_max_len")]
    pub max_len: usize,
    #[serde(default = "default_head_max_len")]
    pub head_max_len: usize,
    #[serde(default)]
    pub temperature: Vec<f64>,
    #[serde(default)]
    pub temperature_by_options: std::collections::BTreeMap<String, f64>,
}

fn default_max_len() -> usize {
    512
}
fn default_head_max_len() -> usize {
    192
}

/// Laya's temperature clamp: a fitted temperature below 0.5 sharpens a coin
/// flip into a certainty (the shipped `choice:11+` bucket is 0.10), so it is
/// refused, as the reference runtime refuses it.
fn clamp_temperature(t: f64) -> f64 {
    if t.is_finite() {
        t.clamp(0.5, 5.0)
    } else {
        1.0
    }
}

impl LayaConfig {
    fn temperature_for(&self, qtype: QType, k: usize) -> f64 {
        let size = match k {
            0..=2 => "2",
            3..=5 => "3-5",
            6..=10 => "6-10",
            _ => "11+",
        };
        let bucket = format!("{}:{size}", qtype.name());
        if let Some(t) = self.temperature_by_options.get(&bucket) {
            return clamp_temperature(*t);
        }
        self.temperature
            .get(qtype as usize)
            .copied()
            .map_or(1.0, clamp_temperature)
    }
}

/// The files a provisioned Laya model directory holds.
pub const LAYA_MODEL_FILES: [&str; 4] = [
    "model.onnx",
    "tokenizer.json",
    "tokenizer_config.json",
    "rl_agent_config.json",
];

/// Whether `dir` holds a Laya model rather than a three-label NLI export.
pub fn is_laya_dir(dir: &Path) -> bool {
    dir.join("rl_agent_config.json").is_file()
}

/// The token ids `build_sequence` needs from the tokenizer.
#[derive(Debug, Clone, Copy)]
pub struct SpecialIds {
    pub cls: u32,
    pub sep: u32,
    pub mask: u32,
    pub pad: u32,
}

/// Port of `laya.common.build_sequence` over pre-tokenized pieces:
/// `head` = tokens of `"<type> question: <instructions>"`, `opts` = tokens of
/// `" " + option` (each capped at 48 upstream), `state` = the state's tokens.
/// Returns (ids, marker positions).
pub fn build_sequence(
    ids: SpecialIds,
    mut head: Vec<u32>,
    opts: Vec<Vec<u32>>,
    state: &[u32],
    max_len: usize,
    head_max_len: usize,
) -> (Vec<u32>, Vec<usize>) {
    let mut opt_ids: Vec<Vec<u32>> = opts
        .into_iter()
        .map(|o| {
            let mut v = Vec::with_capacity(o.len() + 1);
            v.push(ids.mask);
            v.extend(o);
            v
        })
        .collect();
    let total = |o: &[Vec<u32>]| o.iter().map(Vec::len).sum::<usize>() as isize;
    let mut opt_budget = head_max_len as isize - total(&opt_ids);
    if opt_budget < 16 {
        let per = 4.max((head_max_len.saturating_sub(16)) / opt_ids.len().max(1));
        for o in &mut opt_ids {
            o.truncate(per);
        }
        opt_budget = head_max_len as isize - total(&opt_ids);
    }
    head.truncate(opt_budget.max(8) as usize);
    let mut seq = Vec::with_capacity(max_len);
    seq.push(ids.cls);
    seq.extend(head);
    seq.push(ids.sep);
    let mut markers = Vec::with_capacity(opt_ids.len());
    for o in opt_ids {
        markers.push(seq.len());
        seq.extend(o);
    }
    seq.push(ids.sep);
    let room = max_len.saturating_sub(seq.len() + 1);
    seq.extend(&state[..state.len().min(room)]);
    seq.push(ids.sep);
    seq.truncate(max_len);
    markers.retain(|m| *m < max_len);
    (seq, markers)
}

/// Softmax of `logits / t`.
fn softmax_t(logits: &[f32], t: f64) -> Vec<f32> {
    let z: Vec<f64> = logits.iter().map(|l| f64::from(*l) / t).collect();
    let max = z.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let e: Vec<f64> = z.iter().map(|v| (v - max).exp()).collect();
    let sum: f64 = e.iter().sum();
    e.iter().map(|v| (v / sum) as f32).collect()
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
    use crate::Result;
    use crate::nli::{Nli, NliJudgment};

    pub struct LayaModel {
        tokenizer: Tokenizer,
        session: Mutex<Session>,
        cfg: LayaConfig,
        ids: SpecialIds,
        /// The mask token's text, scrubbed out of every input as the
        /// reference runtime does.
        mask_token: String,
    }

    fn laya_err(e: impl std::fmt::Display) -> crate::Error {
        crate::Error::Embedding(format!("laya: {e}"))
    }

    impl LayaModel {
        pub fn from_dir(dir: &Path) -> Result<Self> {
            let cfg: LayaConfig = serde_json::from_slice(
                &std::fs::read(dir.join("rl_agent_config.json"))
                    .map_err(|e| laya_err(format!("reading rl_agent_config.json: {e}")))?,
            )?;
            let mut builder = Session::builder().map_err(laya_err)?;
            builder = builder
                .with_execution_providers(crate::onnx::execution_providers())
                .map_err(laya_err)?;
            if let Some(t) = crate::onnx::intra_threads() {
                builder = builder.with_intra_threads(t).map_err(laya_err)?;
            }
            builder = builder.with_memory_pattern(false).map_err(laya_err)?;
            let session = builder
                .commit_from_file(dir.join("model.onnx"))
                .map_err(laya_err)?;
            let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(laya_err)?;
            // Special tokens are the tokenizer's own: ModernBERT spells them
            // [CLS]/[SEP]/[MASK]/[PAD], mmBERT (the multilingual checkpoint)
            // <bos>/<eos>/<mask>/<pad>. tokenizer_config.json names them;
            // without it the BERT spellings are assumed.
            let tcfg: serde_json::Value = std::fs::read(dir.join("tokenizer_config.json"))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            let name = |key: &str, default: &str| tcfg[key].as_str().unwrap_or(default).to_string();
            let mask_token = name("mask_token", "[MASK]");
            let id = |t: &str| {
                tokenizer
                    .token_to_id(t)
                    .ok_or_else(|| laya_err(format!("tokenizer lacks {t}")))
            };
            let ids = SpecialIds {
                cls: id(&name("cls_token", "[CLS]"))?,
                sep: id(&name("sep_token", "[SEP]"))?,
                mask: id(&mask_token)?,
                pad: id(&name("pad_token", "[PAD]"))?,
            };
            Ok(Self {
                tokenizer,
                session: Mutex::new(session),
                cfg,
                ids,
                mask_token,
            })
        }

        pub fn config(&self) -> &LayaConfig {
            &self.cfg
        }

        fn encode(&self, text: &str) -> Result<Vec<u32>> {
            Ok(self
                .tokenizer
                .encode(text.replace(self.mask_token.as_str(), " "), false)
                .map_err(laya_err)?
                .get_ids()
                .to_vec())
        }

        /// The model's input ids and marker positions for one (state,
        /// question) row — public so the parity test can compare them with
        /// the Python reference token for token.
        pub fn sequence(&self, state: &str, q: &Question) -> Result<(Vec<u32>, Vec<usize>)> {
            let head = self.encode(&format!("{} question: {}", q.qtype.name(), q.instructions))?;
            let mut opts = Vec::with_capacity(q.options.len());
            for o in &q.options {
                let mut t = self.encode(&format!(" {o}"))?;
                t.truncate(48);
                opts.push(t);
            }
            let state = self.encode(state)?;
            let (seq, markers) = build_sequence(
                self.ids,
                head,
                opts,
                &state,
                self.cfg.max_len,
                self.cfg.head_max_len,
            );
            if markers.len() != q.options.len() {
                return Err(laya_err(format!(
                    "options exceed head_max_len={}",
                    self.cfg.head_max_len
                )));
            }
            Ok((seq, markers))
        }

        /// Answer every (state, question) row: one probability per option,
        /// temperature-scaled. Rows run in batches of [`crate::onnx::batch`].
        pub fn decide(&self, rows: &[(String, &Question)]) -> Result<Vec<Vec<f32>>> {
            let mut out = Vec::with_capacity(rows.len());
            for chunk in rows.chunks(crate::onnx::batch()) {
                out.extend(self.decide_batch(chunk)?);
            }
            Ok(out)
        }

        fn decide_batch(&self, rows: &[(String, &Question)]) -> Result<Vec<Vec<f32>>> {
            if rows.is_empty() {
                return Ok(Vec::new());
            }
            let mut built = Vec::with_capacity(rows.len());
            for (state, q) in rows {
                built.push(self.sequence(state, q)?);
            }
            let n = built.len();
            let len = built.iter().map(|(s, _)| s.len()).max().unwrap_or(0);
            let kmax = built.iter().map(|(_, m)| m.len()).max().unwrap_or(0);
            let mut input_ids = vec![i64::from(self.ids.pad); n * len];
            let mut mask = vec![0i64; n * len];
            let mut mpos = vec![0i64; n * kmax];
            let mut mmask = vec![false; n * kmax];
            let mut qtype = Vec::with_capacity(n);
            for (i, ((seq, markers), (_, q))) in built.iter().zip(rows).enumerate() {
                for (j, t) in seq.iter().enumerate() {
                    input_ids[i * len + j] = i64::from(*t);
                    mask[i * len + j] = 1;
                }
                for (j, m) in markers.iter().enumerate() {
                    mpos[i * kmax + j] = *m as i64;
                    mmask[i * kmax + j] = true;
                }
                qtype.push(q.qtype as i64);
            }
            let inputs = ort::inputs![
                "input_ids" => Value::from_array(Array::from_shape_vec((n, len), input_ids).map_err(laya_err)?).map_err(laya_err)?,
                "attention_mask" => Value::from_array(Array::from_shape_vec((n, len), mask).map_err(laya_err)?).map_err(laya_err)?,
                "marker_pos" => Value::from_array(Array::from_shape_vec((n, kmax), mpos).map_err(laya_err)?).map_err(laya_err)?,
                "marker_mask" => Value::from_array(Array::from_shape_vec((n, kmax), mmask).map_err(laya_err)?).map_err(laya_err)?,
                "qtype" => Value::from_array(Array::from_shape_vec(n, qtype).map_err(laya_err)?).map_err(laya_err)?,
            ];
            let mut session = self.session.locked();
            let outputs = session.run(inputs).map_err(laya_err)?;
            let logits = outputs
                .get("logits")
                .ok_or_else(|| laya_err("model output lacks 'logits'"))?
                .try_extract_array::<f32>()
                .map_err(laya_err)?;
            let logits = logits.to_shape((n, kmax)).map_err(laya_err)?;
            Ok(built
                .iter()
                .zip(rows)
                .enumerate()
                .map(|(i, ((_, markers), (_, q)))| {
                    let k = markers.len();
                    let row: Vec<f32> = (0..k).map(|j| logits[[i, j]]).collect();
                    softmax_t(&row, self.cfg.temperature_for(q.qtype, k))
                })
                .collect())
        }
    }

    /// Laya behind the three-way [`Nli`] contract: the XNLI question over a
    /// `{"premise", "hypothesis"}` state.
    pub struct LayaNli {
        model: LayaModel,
        question: Question,
        /// The model directory's name (see [`Nli::model_id`]).
        id: String,
    }

    impl LayaNli {
        pub fn from_dir(dir: &Path) -> Result<Self> {
            Ok(Self {
                model: LayaModel::from_dir(dir)?,
                question: nli_question(),
                id: crate::nli::dir_model_id(dir),
            })
        }

        pub fn model(&self) -> &LayaModel {
            &self.model
        }
    }

    /// The state [`LayaNli`] asks about for one pair.
    pub fn nli_state(premise: &str, hypothesis: &str) -> String {
        state_json(&[
            ("premise", StateValue::Text(premise.to_string())),
            ("hypothesis", StateValue::Text(hypothesis.to_string())),
        ])
    }

    impl Nli for LayaNli {
        fn model_id(&self) -> String {
            self.id.clone()
        }

        fn judge(&self, pairs: &[(String, String)]) -> Result<Vec<NliJudgment>> {
            let rows: Vec<(String, &Question)> = pairs
                .iter()
                .map(|(p, h)| (nli_state(p, h), &self.question))
                .collect();
            Ok(self
                .model
                .decide(&rows)?
                .into_iter()
                .map(|p| NliJudgment {
                    entailment: p[0],
                    neutral: p[1],
                    contradiction: p[2],
                })
                .collect())
        }
    }
}

#[cfg(feature = "fastembed")]
pub use fast::{LayaModel, LayaNli, nli_state};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_json_matches_python_dumps() {
        // json.dumps({"premise": "a \"b\"", "hypothesis": "ü\n", "links": ["x", "y"]},
        //            ensure_ascii=False)
        let s = state_json(&[
            ("premise", StateValue::Text("a \"b\"".into())),
            ("hypothesis", StateValue::Text("ü\n".into())),
            ("links", StateValue::List(vec!["x".into(), "y".into()])),
        ]);
        assert_eq!(
            s,
            r#"{"premise": "a \"b\"", "hypothesis": "ü\n", "links": ["x", "y"]}"#
        );
    }

    #[test]
    fn build_sequence_lays_out_markers_and_truncates_state() {
        let ids = SpecialIds {
            cls: 1,
            sep: 2,
            mask: 3,
            pad: 0,
        };
        let (seq, markers) = build_sequence(
            ids,
            vec![10, 11],
            vec![vec![20], vec![21, 22]],
            &[30; 50],
            16,
            192,
        );
        assert_eq!(&seq[..4], &[1, 10, 11, 2]);
        assert_eq!(markers, vec![4, 6]);
        assert_eq!(&seq[4..10], &[3, 20, 3, 21, 22, 2]);
        assert_eq!(seq.len(), 16);
        assert_eq!(*seq.last().unwrap(), 2);
    }

    /// Parity against the Python reference (`laya.common.build_sequence` +
    /// onnxruntime): same token ids, same marker positions, same
    /// probabilities. The fixture is a JSON array of {premise, hypothesis,
    /// ids, markers, probs} rows. Run with
    /// `LAYA_DIR=… LAYA_REF=… cargo test -p engram-core -F fastembed laya_matches -- --ignored`.
    #[test]
    #[ignore = "needs a downloaded Laya model and a Python reference fixture"]
    #[cfg(feature = "fastembed")]
    fn laya_matches_python_reference() {
        let dir = std::path::PathBuf::from(std::env::var("LAYA_DIR").expect("LAYA_DIR"));
        let rows: Vec<serde_json::Value> = serde_json::from_slice(
            &std::fs::read(std::env::var("LAYA_REF").expect("LAYA_REF")).unwrap(),
        )
        .unwrap();
        let m = LayaModel::from_dir(&dir).unwrap();
        let q = nli_question();
        for r in rows {
            let state = nli_state(
                r["premise"].as_str().unwrap(),
                r["hypothesis"].as_str().unwrap(),
            );
            let (ids, markers) = m.sequence(&state, &q).unwrap();
            let want: Vec<u32> = serde_json::from_value(r["ids"].clone()).unwrap();
            let want_m: Vec<usize> = serde_json::from_value(r["markers"].clone()).unwrap();
            assert_eq!(ids, want, "token ids differ");
            assert_eq!(markers, want_m);
            let p = m.decide(&[(state, &q)]).unwrap().remove(0);
            let want_p: Vec<f32> = serde_json::from_value(r["probs"].clone()).unwrap();
            for (a, b) in p.iter().zip(&want_p) {
                assert!((a - b).abs() < 1e-3, "probs {p:?} vs {want_p:?}");
            }
        }
    }

    #[test]
    fn temperatures_bucket_and_clamp() {
        let cfg: LayaConfig = serde_json::from_str(
            r#"{"temperature": [1.6, 1.2, 2.0], "temperature_by_options": {"choice:3-5": 1.76, "choice:11+": 0.1}}"#,
        )
        .unwrap();
        assert!((cfg.temperature_for(QType::Choice, 3) - 1.76).abs() < 1e-9);
        assert!((cfg.temperature_for(QType::Choice, 12) - 0.5).abs() < 1e-9);
        assert!((cfg.temperature_for(QType::Noul, 2) - 2.0).abs() < 1e-9);
    }
}
