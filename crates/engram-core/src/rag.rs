//! Embedding generation. The store owns vector *storage* (sqlite-vec); this
//! module owns turning text into vectors. An `Embedder` trait lets the daemon
//! use real local embeddings (`fastembed`, behind the `fastembed` feature)
//! while tests use a deterministic fake — no ONNX, no network (PLAN approach #2).

use crate::Result;

/// Embedding dimensionality. Matches `bge-small-en-v1.5` and the `vec_nodes`
/// table width. Changing this requires rebuilding the vector table.
pub const EMBED_DIM: usize = 384;

pub trait Embedder: Send + Sync {
    fn dim(&self) -> usize;
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self
            .embed(std::slice::from_ref(&text.to_string()))?
            .pop()
            .expect("embed returns one vector per input"))
    }

    /// Deterministic stand-in vectors (`--fake-embeddings`)? Bulk maintenance
    /// passes (composition re-embeds) must not run with a fake over an
    /// existing graph — they would overwrite real vectors with noise. The
    /// brief hook routinely opens real DBs with the fake embedder.
    fn is_fake(&self) -> bool {
        false
    }
}

/// Deterministic, dependency-free embedder for tests and offline fallback.
/// Same text → same unit vector; texts sharing bytes get higher cosine
/// similarity. Not semantic — exercises the plumbing and ranking, not meaning.
pub struct FakeEmbedder {
    dim: usize,
}

impl Default for FakeEmbedder {
    fn default() -> Self {
        Self { dim: EMBED_DIM }
    }
}

impl Embedder for FakeEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn is_fake(&self) -> bool {
        true
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; self.dim];
                for &b in t.to_lowercase().as_bytes() {
                    v[b as usize % self.dim] += 1.0;
                }
                normalize(&mut v);
                v
            })
            .collect())
    }
}

fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// The precision layer of the local cortex (PLAN §7A): a cross-encoder that
/// re-scores retrieval candidates against the query. Bi-encoder recall
/// (bge-small) casts the net; this sharpens the top of it. Optional — the
/// engine falls back to plain hybrid order when absent (tests, offline
/// first run, `--fake-embeddings`).
pub trait Reranker: Send + Sync {
    /// Raw relevance logits, one per document, higher = more relevant.
    fn rank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>>;
}

#[cfg(feature = "fastembed")]
mod fast {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use fastembed::{
        EmbeddingModel, InitOptions, InitOptionsUserDefined, Pooling, QuantizationMode,
        TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
    };

    use super::*;

    /// The five files that make up the bge-small-en-v1.5 model on disk.
    const MODEL_FILES: [&str; 5] = [
        "model.onnx",
        "tokenizer.json",
        "config.json",
        "special_tokens_map.json",
        "tokenizer_config.json",
    ];

    /// Local ONNX embeddings via `fastembed` (`bge-small-en-v1.5`, 384-dim).
    ///
    /// Loading order: if a local model directory has all the files, load from
    /// there (deterministic, offline, no `hf_hub`). Otherwise fall back to
    /// `fastembed`'s `hf_hub` download. We prefer the explicit directory because
    /// `hf_hub`'s Xet CDN path is flaky, and a daemon shouldn't block startup on
    /// a network fetch. `model_dir()` resolves `ENGRAM_MODEL_DIR`, else
    /// `~/.cache/engram/bge-small-en-v1.5`.
    ///
    /// `embed` takes `&mut self`; a `Mutex` keeps the shared `&self` trait method
    /// and serializes inference (fine for a local single-user daemon).
    pub struct FastEmbedder {
        model: Mutex<TextEmbedding>,
    }

    impl FastEmbedder {
        pub fn new() -> Result<Self> {
            let model = match model_dir().filter(|d| has_all_files(d)) {
                Some(dir) => Self::from_dir(&dir)?,
                None => {
                    // Keep the hf_hub download out of the project: fastembed's
                    // default cache is ./.fastembed_cache in the cwd (i.e. the
                    // user's repo). Cache machine-wide next to our own model
                    // dir instead, so every repo shares one copy.
                    let mut opts = InitOptions::new(EmbeddingModel::BGESmallENV15)
                        .with_show_download_progress(false);
                    if let Some(cache) = shared_cache_dir() {
                        opts = opts.with_cache_dir(cache);
                    }
                    TextEmbedding::try_new(opts).map_err(emb_err)?
                }
            };
            Ok(Self {
                model: Mutex::new(model),
            })
        }

        fn from_dir(dir: &Path) -> Result<TextEmbedding> {
            let read = |name: &str| {
                std::fs::read(dir.join(name))
                    .map_err(|e| crate::Error::Embedding(format!("reading {name}: {e}")))
            };
            let model = UserDefinedEmbeddingModel {
                onnx_file: read("model.onnx")?,
                external_initializers: vec![],
                tokenizer_files: TokenizerFiles {
                    tokenizer_file: read("tokenizer.json")?,
                    config_file: read("config.json")?,
                    special_tokens_map_file: read("special_tokens_map.json")?,
                    tokenizer_config_file: read("tokenizer_config.json")?,
                },
                pooling: Some(Pooling::Cls), // bge-small uses CLS pooling
                quantization: QuantizationMode::None,
                output_key: None,
            };
            TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::default())
                .map_err(emb_err)
        }
    }

    impl Embedder for FastEmbedder {
        fn dim(&self) -> usize {
            EMBED_DIM
        }

        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.model
                .lock()
                .expect("embedder mutex")
                .embed(texts, None)
                .map_err(emb_err)
        }
    }

    fn emb_err(e: impl std::fmt::Display) -> crate::Error {
        crate::Error::Embedding(e.to_string())
    }

    /// Local ONNX cross-encoder reranking (`jina-reranker-v1-turbo-en`, 38M —
    /// small enough for CPU, English-only like bge-small-en). Same loading
    /// order as the embedder: an explicit model directory wins (deterministic,
    /// offline — `ENGRAM_RERANKER_DIR`, else
    /// `~/.cache/engram/jina-reranker-v1-turbo-en`), `hf_hub` download is the
    /// fallback. Init failure is the caller's to soften (the engine simply
    /// keeps hybrid order without a reranker).
    pub struct FastReranker {
        model: Mutex<fastembed::TextRerank>,
    }

    impl FastReranker {
        pub fn new() -> Result<Self> {
            let model = match reranker_model_dir().filter(|d| has_all_files(d)) {
                Some(dir) => Self::from_dir(&dir)?,
                None => {
                    let mut opts = fastembed::RerankInitOptions::new(
                        fastembed::RerankerModel::JINARerankerV1TurboEn,
                    )
                    .with_show_download_progress(false);
                    if let Some(cache) = shared_cache_dir() {
                        opts = opts.with_cache_dir(cache);
                    }
                    fastembed::TextRerank::try_new(opts).map_err(emb_err)?
                }
            };
            Ok(Self {
                model: Mutex::new(model),
            })
        }

        fn from_dir(dir: &Path) -> Result<fastembed::TextRerank> {
            let read = |name: &str| {
                std::fs::read(dir.join(name))
                    .map_err(|e| crate::Error::Embedding(format!("reading {name}: {e}")))
            };
            let model = fastembed::UserDefinedRerankingModel::new(
                read("model.onnx")?,
                TokenizerFiles {
                    tokenizer_file: read("tokenizer.json")?,
                    config_file: read("config.json")?,
                    special_tokens_map_file: read("special_tokens_map.json")?,
                    tokenizer_config_file: read("tokenizer_config.json")?,
                },
            );
            fastembed::TextRerank::try_new_from_user_defined(
                model,
                fastembed::RerankInitOptionsUserDefined::default(),
            )
            .map_err(emb_err)
        }
    }

    /// Where the reranker model lives (`ENGRAM_RERANKER_DIR` override).
    pub fn reranker_model_dir() -> Option<PathBuf> {
        if let Ok(dir) = std::env::var("ENGRAM_RERANKER_DIR") {
            return Some(PathBuf::from(dir));
        }
        home().map(|h| h.join(".cache/engram/jina-reranker-v1-turbo-en"))
    }

    impl Reranker for FastReranker {
        fn rank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>> {
            let docs: Vec<&str> = documents.iter().map(String::as_str).collect();
            let mut results = self
                .model
                .lock()
                .expect("reranker mutex")
                .rerank(query, docs, false, None)
                .map_err(emb_err)?;
            // fastembed returns best-first; restore input order for the caller.
            results.sort_by_key(|r| r.index);
            Ok(results.into_iter().map(|r| r.score).collect())
        }
    }

    /// Where the embedding model lives (`ENGRAM_MODEL_DIR` override) — also
    /// reported by `/system` so the pane can show real paths.
    pub fn model_dir() -> Option<PathBuf> {
        if let Ok(dir) = std::env::var("ENGRAM_MODEL_DIR") {
            return Some(PathBuf::from(dir));
        }
        home().map(|h| h.join(".cache/engram/bge-small-en-v1.5"))
    }

    /// Machine-wide cache for fastembed's own hf_hub downloads.
    fn shared_cache_dir() -> Option<PathBuf> {
        home().map(|h| h.join(".cache/engram/fastembed"))
    }

    fn home() -> Option<PathBuf> {
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()
            .map(PathBuf::from)
    }

    fn has_all_files(dir: &Path) -> bool {
        MODEL_FILES.iter().all(|f| dir.join(f).is_file())
    }
}

#[cfg(feature = "fastembed")]
pub use fast::{FastEmbedder, FastReranker, model_dir, reranker_model_dir};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_is_deterministic_and_normalized() {
        let e = FakeEmbedder::default();
        let a = e.embed_one("auth flow decision").unwrap();
        let b = e.embed_one("auth flow decision").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), EMBED_DIM);
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-4,
            "expected unit vector, got {norm}"
        );
    }

    #[test]
    fn similar_text_more_similar_than_unrelated() {
        let e = FakeEmbedder::default();
        let q = e.embed_one("sqlite database storage").unwrap();
        let near = e.embed_one("sqlite database storage engine").unwrap();
        let far = e.embed_one("zzzzzzzz").unwrap();
        assert!(cosine(&q, &near) > cosine(&q, &far));
    }

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }
}
