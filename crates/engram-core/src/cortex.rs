//! Cortex model selection: which local models power embeddings, reranking and
//! NLI, chosen per machine in `~/.engram/models.json`. This module owns the
//! config shape, presets and path resolution; the CLI downloads, rag/nli load.
//! Reranker and NLI swap freely; a new embedder means a new vector space, so
//! `ensure_embed_model` re-embeds the graph. Thresholds were tuned on
//! bge-small: with another embedder, suspect-queue quality is unvalidated.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The three swappable cortex layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Embedding,
    Reranker,
    Nli,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Embedding => "embedding",
            Role::Reranker => "reranker",
            Role::Nli => "nli",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "embedding" => Ok(Role::Embedding),
            "reranker" => Ok(Role::Reranker),
            "nli" => Ok(Role::Nli),
            _ => Err(Error::Parse {
                kind: "Role",
                value: s.to_string(),
            }),
        }
    }
}

/// One model's provisioning identity: a directory-shaped base URL (Hugging
/// Face `…/resolve/main` style) the files download from, into
/// `~/.cache/engram/<name>/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    pub name: String,
    pub base_url: String,
    /// Repo-relative source of the ONNX graph (quantized exports usually live
    /// under `onnx/`); always saved locally as plain `model.onnx`.
    #[serde(default = "default_model_file")]
    pub model_file: String,
    /// Embedding role only: output width. Drives the vector-store rebuild.
    #[serde(default)]
    pub dim: Option<usize>,
    /// Embedding role only: `cls` (BERT/bge family) or `mean`.
    #[serde(default)]
    pub pooling: Option<String>,
    /// One line the pane shows under the picker: what this model trades.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// NLI role only: the directory layout. `None` = a three-label NLI
    /// export (`config.json` with `id2label`); `"laya"` = a Laya
    /// typed-decision model (`rl_agent_config.json` + `tokenizer_config.json`,
    /// see [`crate::laya`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
}

impl ModelSpec {
    /// Whether this spec provisions a Laya typed-decision model.
    pub fn is_laya(&self) -> bool {
        self.layout.as_deref() == Some("laya")
    }
}

fn default_model_file() -> String {
    "onnx/model_quantized.onnx".to_string()
}

/// `~/.engram/models.json`. A missing file or field means the built-in
/// default for that role — the config only records deviations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CortexConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<ModelSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reranker: Option<ModelSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nli: Option<ModelSpec>,
}

impl CortexConfig {
    pub fn get(&self, role: Role) -> Option<&ModelSpec> {
        match role {
            Role::Embedding => self.embedding.as_ref(),
            Role::Reranker => self.reranker.as_ref(),
            Role::Nli => self.nli.as_ref(),
        }
    }

    pub fn set(&mut self, role: Role, spec: Option<ModelSpec>) {
        match role {
            Role::Embedding => self.embedding = spec,
            Role::Reranker => self.reranker = spec,
            Role::Nli => self.nli = spec,
        }
    }

    /// The spec actually in force for a role: the configured one, or the
    /// built-in default.
    pub fn effective(&self, role: Role) -> ModelSpec {
        self.get(role)
            .cloned()
            .unwrap_or_else(|| presets(role)[0].clone())
    }
}

/// Where the machine-level model selection lives (rides `ENGRAM_HOME` like
/// the registry).
pub fn config_path() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("ENGRAM_HOME") {
        return Some(PathBuf::from(home).join("models.json"));
    }
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(|h| PathBuf::from(h).join(".engram/models.json"))
}

/// Load the selection; any unreadable/absent config is the defaults.
pub fn load() -> CortexConfig {
    config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(cfg: &CortexConfig) -> Result<()> {
    let path = config_path().ok_or_else(|| Error::Io("no home directory".into()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::Io(format!("creating {}: {e}", dir.display())))?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(cfg)?)
        .map_err(|e| Error::Io(format!("writing {}: {e}", path.display())))
        .map(|_| ())
}

/// Known-good presets per role, default first. All Hugging Face ONNX exports
/// with the file layout the loaders expect.
pub fn presets(role: Role) -> Vec<ModelSpec> {
    let hf = |repo: &str| format!("https://huggingface.co/{repo}/resolve/main");
    match role {
        Role::Embedding => vec![
            // fp32, pinned: every receipt in `eval/results/` used fp32, and machines that
            // already had a model dir never picked up int8. int8 would save ~100 MB but is
            // a retrieval change, so it needs a `--ladder` run first.
            ModelSpec {
                name: crate::rag::DEFAULT_EMBED_MODEL.to_string(),
                base_url: hf("Xenova/bge-small-en-v1.5"),
                model_file: "onnx/model.onnx".into(),
                dim: Some(384),
                pooling: Some("cls".into()),
                description: None,
                layout: None,
            },
            ModelSpec {
                name: "bge-base-en-v1.5".into(),
                base_url: hf("Xenova/bge-base-en-v1.5"),
                model_file: default_model_file(),
                dim: Some(768),
                pooling: Some("cls".into()),
                description: None,
                layout: None,
            },
            ModelSpec {
                name: "all-MiniLM-L6-v2".into(),
                base_url: hf("Xenova/all-MiniLM-L6-v2"),
                model_file: default_model_file(),
                dim: Some(384),
                pooling: Some("mean".into()),
                description: None,
                layout: None,
            },
        ],
        Role::Reranker => vec![
            ModelSpec {
                name: "jina-reranker-v1-turbo-en".into(),
                base_url: hf("jinaai/jina-reranker-v1-turbo-en"),
                model_file: "onnx/model.onnx".into(),
                dim: None,
                pooling: None,
                description: None,
                layout: None,
            },
            ModelSpec {
                name: "bge-reranker-base".into(),
                base_url: hf("Xenova/bge-reranker-base"),
                model_file: default_model_file(),
                dim: None,
                pooling: None,
                description: None,
                layout: None,
            },
        ],
        // First entry is the default. tasksource stays it after the Laya bench: Laya
        // reads compound sentences better but flags more dismissed pairs on this
        // repo's graph (28% vs 19% at 0.80) at 15× the cost
        // (eval/results/2026-09-24-shapes-*, -realgraph-*). Old selections of removed
        // models still resolve as custom specs.
        Role::Nli => vec![
            ModelSpec {
                name: "deberta-v3-small-tasksource-nli".into(),
                base_url: hf("techtheist/deberta-v3-small-tasksource-nli-onnx"),
                model_file: default_model_file(),
                dim: None,
                pooling: None,
                description: Some(
                    "Fast and careful (172 MB, ~10 ms a pair): the fewest false alarms on real notes.".into(),
                ),
                layout: None,
            },
            ModelSpec {
                name: "laya-en-int4".into(),
                base_url: hf("techtheist/laya-onnx") + "/en",
                model_file: "model_int4.onnx".into(),
                dim: None,
                pooling: None,
                description: Some(
                    "Laya, English, int4 (262 MB, ~15× slower): catches compound contradictions, flags more real pairs.".into(),
                ),
                layout: Some("laya".into()),
            },
            ModelSpec {
                name: "laya-multilingual-int8".into(),
                base_url: hf("techtheist/laya-onnx") + "/multilingual",
                model_file: "model_int8.onnx".into(),
                dim: None,
                pooling: None,
                description: Some(
                    "Laya, 100+ languages, int8 (873 MB): for notes written in other languages; noisier on English.".into(),
                ),
                layout: Some("laya".into()),
            },
        ],
    }
}

/// Every model's on-disk home: `~/.cache/engram/<name>` — the same layout the
/// pre-selection loaders already used, so existing caches keep working.
pub fn cache_dir(name: &str) -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(|h| PathBuf::from(h).join(".cache/engram").join(name))
}

/// The (local filename, source URL) pairs a spec needs on disk. Embeddings
/// and rerankers ride fastembed's five-file layout; NLI needs three; a Laya
/// model needs four (its own config and the tokenizer's, no `config.json`).
pub fn spec_files(role: Role, spec: &ModelSpec) -> Vec<(String, String)> {
    let src = |rel: &str| format!("{}/{}", spec.base_url.trim_end_matches('/'), rel);
    if spec.is_laya() {
        return crate::laya::LAYA_MODEL_FILES
            .iter()
            .map(|f| {
                let from = if *f == "model.onnx" {
                    spec.model_file.as_str()
                } else {
                    f
                };
                (f.to_string(), src(from))
            })
            .collect();
    }
    let mut files = vec![
        ("model.onnx".to_string(), src(&spec.model_file)),
        ("tokenizer.json".to_string(), src("tokenizer.json")),
        ("config.json".to_string(), src("config.json")),
    ];
    if role != Role::Nli {
        files.push((
            "special_tokens_map.json".to_string(),
            src("special_tokens_map.json"),
        ));
        files.push((
            "tokenizer_config.json".to_string(),
            src("tokenizer_config.json"),
        ));
    }
    files
}
