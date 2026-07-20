//! Cortex model selection (PLAN §7A / Intent "user-selectable cortex
//! models"): which local models power each layer — embeddings (recall),
//! reranker (precision), NLI (logic) — chosen per machine in
//! `~/.engram/models.json`. This module owns the config shape, the known-good
//! presets, and path resolution; downloading stays in the CLI (core is
//! HTTP-free), loading stays in rag/nli.
//!
//! The compatibility contract splits by layer: reranker/NLI are stateless
//! swaps; embeddings are the hard case — a different model means a different
//! vector space (and possibly width), so the engine's `ensure_embed_model`
//! guard rebuilds vector storage and re-embeds the whole graph when the
//! active identity diverges from what a store carries. The calibrated cosine
//! thresholds (0.90 dupe / 0.88 suspect / 0.85 promotion) were tuned on
//! bge-small — a swapped embedding model shifts that distribution, so treat
//! suspect-queue quality as unvalidated until re-tuned (surfaced in the UI).

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
            ModelSpec {
                name: crate::rag::DEFAULT_EMBED_MODEL.to_string(),
                base_url: hf("Xenova/bge-small-en-v1.5"),
                model_file: default_model_file(),
                dim: Some(384),
                pooling: Some("cls".into()),
            },
            ModelSpec {
                name: "bge-base-en-v1.5".into(),
                base_url: hf("Xenova/bge-base-en-v1.5"),
                model_file: default_model_file(),
                dim: Some(768),
                pooling: Some("cls".into()),
            },
            ModelSpec {
                name: "all-MiniLM-L6-v2".into(),
                base_url: hf("Xenova/all-MiniLM-L6-v2"),
                model_file: default_model_file(),
                dim: Some(384),
                pooling: Some("mean".into()),
            },
        ],
        Role::Reranker => vec![
            ModelSpec {
                name: "jina-reranker-v1-turbo-en".into(),
                base_url: hf("jinaai/jina-reranker-v1-turbo-en"),
                model_file: "onnx/model.onnx".into(),
                dim: None,
                pooling: None,
            },
            ModelSpec {
                name: "bge-reranker-base".into(),
                base_url: hf("Xenova/bge-reranker-base"),
                model_file: default_model_file(),
                dim: None,
                pooling: None,
            },
        ],
        Role::Nli => vec![ModelSpec {
            name: "nli-deberta-v3-small".into(),
            base_url: hf("Xenova/nli-deberta-v3-small"),
            model_file: default_model_file(),
            dim: None,
            pooling: None,
        }],
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
/// and rerankers ride fastembed's five-file layout; NLI needs three.
pub fn spec_files(role: Role, spec: &ModelSpec) -> Vec<(String, String)> {
    let src = |rel: &str| format!("{}/{}", spec.base_url.trim_end_matches('/'), rel);
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
