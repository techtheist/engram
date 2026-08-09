//! GPU embeddings via a local Ollama server — an eval-harness runtime swap,
//! never the daemon. Same `bge-small-en-v1.5` weights as the shipped
//! fastembed path (GGUF F16 export, CLS pooling — `bert.pooling_type 2` —
//! L2-normalised output), served by llama.cpp on Metal. Exists because the
//! LongMemEval write path is ~99% embedder time on CPU (sampled live,
//! 2026-08-08: `embed_node` → MLAS sgemm); the reranker and NLI stay on the
//! CPU cortex — Ollama has no cross-encoder or classifier API, and both are
//! noise in that profile anyway.

use engram_core::rag::{DEFAULT_EMBED_MODEL, EMBED_DIM, Embedder};
use engram_core::{Error, Result};
use serde::Deserialize;

pub const DEFAULT_URL: &str = "http://localhost:11434";
/// The F16 export keeps quantisation drift out of the comparison — the only
/// difference from the shipped path is the runtime, not the weights.
pub const DEFAULT_MODEL: &str = "hf.co/CompendiumLabs/bge-small-en-v1.5-gguf:F16";

/// Which embedding API the server at the URL speaks. `ollama serve` has
/// `/api/embed`; a bare `llama-server` (worth driving directly — ollama
/// pins embedding runners to one parallel slot, a bare server takes `-np 8`
/// and batches concurrent requests on the GPU) has OpenAI's
/// `/v1/embeddings`. Detected once at startup by probing.
#[derive(Clone, Copy, PartialEq)]
enum Api {
    Ollama,
    OpenAi,
}

pub struct OllamaEmbedder {
    agent: ureq::Agent,
    url: String,
    model: String,
    api: Api,
    /// A bare `llama-server` rejects inputs past its physical batch instead
    /// of truncating them at the model's 512-token window the way ollama
    /// (and the shipped fastembed path) do — and ~21% of LongMemEval turns
    /// are that long. Batches it refuses re-route here: an ollama server
    /// whose truncation already grades identically to the shipped path.
    fallback_url: Option<String>,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[derive(Deserialize)]
struct OpenAiEmbedResponse {
    data: Vec<OpenAiEmbedding>,
}

#[derive(Deserialize)]
struct OpenAiEmbedding {
    index: usize,
    embedding: Vec<f32>,
}

impl OllamaEmbedder {
    /// Probes the server with a real embed so a missing server, missing
    /// model, or wrong-width model fails loudly at startup, not mid-run.
    /// `ENGRAM_EVAL_OLLAMA_URL` / `ENGRAM_EVAL_OLLAMA_MODEL` override the
    /// defaults.
    pub fn new() -> Result<Self> {
        let url = std::env::var("ENGRAM_EVAL_OLLAMA_URL").unwrap_or_else(|_| DEFAULT_URL.into());
        let model =
            std::env::var("ENGRAM_EVAL_OLLAMA_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());
        // Hard timeouts, learned the hard way: a server bounced mid-run left
        // every worker blocked forever on a read that would never return —
        // the run "ran" for 40 minutes at 0% CPU. A timeout turns that into
        // an error, and an error has a fallback path.
        let mut me = Self {
            agent: ureq::AgentBuilder::new()
                .timeout_connect(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(120))
                .build(),
            url,
            model,
            api: Api::Ollama,
            fallback_url: None,
        };
        let probe_input = vec!["dimension probe".to_string()];
        let probe = match me.call(&probe_input) {
            Ok(v) => v,
            Err(_) => {
                me.api = Api::OpenAi;
                let probe = me.call(&probe_input)?;
                // Probe the fallback now too — a missing fallback would
                // otherwise surface hours in, on the first oversized turn.
                let fb = std::env::var("ENGRAM_EVAL_OLLAMA_FALLBACK_URL")
                    .unwrap_or_else(|_| DEFAULT_URL.into());
                me.call_to(&fb, Api::Ollama, &probe_input)?;
                me.fallback_url = Some(fb);
                probe
            }
        };
        let got = probe.first().map(Vec::len).unwrap_or(0);
        if got != EMBED_DIM {
            return Err(Error::Embedding(format!(
                "embedding server model {} returns {got}-dim vectors, need {EMBED_DIM}",
                me.model
            )));
        }
        Ok(me)
    }

    fn call(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        match self.call_to(&self.url, self.api, texts) {
            Ok(v) => Ok(v),
            Err(e) => match &self.fallback_url {
                Some(fb) => self.call_to(fb, Api::Ollama, texts),
                None => Err(e),
            },
        }
    }

    fn call_to(&self, base: &str, api: Api, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let (path, body) = match api {
            Api::Ollama => (
                "/api/embed",
                serde_json::json!({
                    "model": self.model,
                    "input": texts,
                    // A full run takes hours of continuous traffic; never
                    // let the server unload the model between questions.
                    "keep_alive": "60m",
                }),
            ),
            Api::OpenAi => (
                "/v1/embeddings",
                serde_json::json!({ "model": self.model, "input": texts }),
            ),
        };
        let resp = self
            .agent
            .post(&format!("{base}{path}"))
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .map_err(|e| Error::Embedding(format!("embedding server {path}: {e}")))?;
        let vectors = match api {
            Api::Ollama => {
                let parsed: EmbedResponse = serde_json::from_reader(resp.into_reader())
                    .map_err(|e| Error::Embedding(format!("{path} response: {e}")))?;
                parsed.embeddings
            }
            Api::OpenAi => {
                let parsed: OpenAiEmbedResponse = serde_json::from_reader(resp.into_reader())
                    .map_err(|e| Error::Embedding(format!("{path} response: {e}")))?;
                let mut data = parsed.data;
                data.sort_by_key(|d| d.index);
                data.into_iter().map(|d| d.embedding).collect()
            }
        };
        if vectors.len() != texts.len() {
            return Err(Error::Embedding(format!(
                "embedding server returned {} vectors for {} inputs",
                vectors.len(),
                texts.len()
            )));
        }
        Ok(vectors)
    }
}

impl Embedder for OllamaEmbedder {
    fn dim(&self) -> usize {
        EMBED_DIM
    }

    /// Same weights, same identity — stores stamped by this run read as the
    /// default model, which they are.
    fn name(&self) -> &str {
        DEFAULT_EMBED_MODEL
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        const BATCH: usize = 64;
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(BATCH) {
            // Hours-long runs meet transient hiccups — including a server
            // being bounced under the run, which costs ~10 s of model
            // reload. Spaced retries ride that out; only a genuinely down
            // stack ends the run.
            let mut attempt = 0;
            let vectors = loop {
                match self.call(chunk) {
                    Ok(v) => break v,
                    Err(e) if attempt >= 3 => return Err(e),
                    Err(_) => {
                        attempt += 1;
                        std::thread::sleep(std::time::Duration::from_secs(2 * attempt));
                    }
                }
            };
            out.extend(vectors);
        }
        Ok(out)
    }
}
