//! Model loading and provisioning, and the pane's model admin.

use crate::*;

/// One loaded model set — what every engine the process opens is wired to
///.
#[derive(Clone)]
pub(crate) struct ModelSet {
    pub(crate) embedder: Arc<dyn Embedder>,
    pub(crate) reranker: Option<Arc<dyn Reranker>>,
    pub(crate) nli: Option<Arc<dyn Nli>>,
}

/// The live model runtime. The `RwLock` is model selection's hot-swap point
///: `/models` replaces a slot, already-open engines get the new
/// handle pushed in, engines opened later read the current set here.
#[derive(Clone)]
pub(crate) struct Models {
    pub(crate) set: Arc<std::sync::RwLock<ModelSet>>,
}

impl Models {
    pub(crate) fn current(&self) -> ModelSet {
        self.set.reading().clone()
    }
}

/// Load the models named by `~/.engram/models.json`: the embedder always, the
/// reranker and NLI when `cortex`. User selections are provisioned from their
/// recorded URLs into `~/.cache/engram/<name>/` first.
pub(crate) fn load_models(fake_embeddings: bool, cortex: bool) -> anyhow::Result<Models> {
    let cfg = engram_core::cortex::load();
    let embedder: Arc<dyn Embedder> = if fake_embeddings {
        tracing::warn!("using fake embeddings (search quality is degraded)");
        Arc::new(FakeEmbedder::default())
    } else {
        tracing::info!("loading local embedding model…");
        load_embedder(&cfg.effective(engram_core::cortex::Role::Embedding))
            .context("initializing the embedding model")?
    };
    let mut set = ModelSet {
        embedder,
        reranker: None,
        nli: None,
    };
    if cortex && !fake_embeddings {
        // The cortex is an upgrade, never a dependency: any failed load
        // (first run offline, cache wiped) degrades that layer away.
        match load_reranker(&cfg.effective(engram_core::cortex::Role::Reranker)) {
            Ok(r) => set.reranker = Some(r),
            Err(e) => tracing::warn!("reranker unavailable, search keeps hybrid order: {e}"),
        }
        match load_nli(&cfg.effective(engram_core::cortex::Role::Nli)) {
            Ok(n) => set.nli = Some(n),
            Err(e) => tracing::warn!("NLI unavailable, cortex hints disabled: {e}"),
        }
    }
    Ok(Models {
        set: Arc::new(std::sync::RwLock::new(set)),
    })
}

pub(crate) fn is_default_spec(
    role: engram_core::cortex::Role,
    spec: &engram_core::cortex::ModelSpec,
) -> bool {
    engram_core::cortex::presets(role)[0].name == spec.name
}

/// Provision (when needed) and load one embedding spec. The default model
/// keeps `FastEmbedder::new`'s dir-then-hf_hub behavior; anything else loads
/// strictly from its provisioned directory.
pub(crate) fn load_embedder(
    spec: &engram_core::cortex::ModelSpec,
) -> anyhow::Result<Arc<dyn Embedder>> {
    use engram_core::cortex::Role;
    if is_default_spec(Role::Embedding, spec) {
        // Provision the default like any selected model — the curl download
        // into the same dir FastEmbedder::new prefers. fastembed's flaky
        // hf_hub fallback inside new() becomes the last resort, not the plan.
        if let Err(e) = provision(Role::Embedding, spec) {
            tracing::warn!("couldn't pre-provision the embedding model ({e:#}); falling back");
        }
        return Ok(Arc::new(FastEmbedder::new()?));
    }
    let dir = provision(Role::Embedding, spec)?;
    let dim = spec
        .dim
        .with_context(|| format!("model {} has no dim recorded", spec.name))?;
    Ok(Arc::new(FastEmbedder::from_spec(
        &spec.name,
        &dir,
        dim,
        spec.pooling.as_deref() == Some("mean"),
    )?))
}

pub(crate) fn load_reranker(
    spec: &engram_core::cortex::ModelSpec,
) -> anyhow::Result<Arc<dyn Reranker>> {
    use engram_core::cortex::Role;
    if is_default_spec(Role::Reranker, spec) {
        // The long-open asymmetry (Problem 009yrpcyno9p): the NLI model had a
        // curl ensure, the reranker leaned on fastembed's flaky hf_hub
        // download and silently lost the precision layer when it failed.
        // Provision it like everything else; new() prefers the local dir.
        if let Err(e) = provision(Role::Reranker, spec) {
            tracing::warn!("couldn't pre-provision the reranker ({e:#}); falling back");
        }
        return Ok(Arc::new(engram_core::FastReranker::new()?));
    }
    let dir = provision(Role::Reranker, spec)?;
    Ok(Arc::new(engram_core::FastReranker::open_dir(&dir)?))
}

pub(crate) fn load_nli(spec: &engram_core::cortex::ModelSpec) -> anyhow::Result<Arc<dyn Nli>> {
    use engram_core::cortex::Role;
    if is_default_spec(Role::Nli, spec) {
        // The default NLI keeps its ENGRAM_NLI_DIR override + CLI download.
        ensure_nli_model();
        return Ok(Arc::new(engram_core::FastNli::new()?));
    }
    // A selected judge is either a three-label NLI export or a Laya
    // typed-decision model; the directory says which.
    let dir = provision(Role::Nli, spec)?;
    Ok(Arc::from(engram_core::nli::load_dir(&dir)?))
}

/// Make sure a spec's files exist under `~/.cache/engram/<name>/`,
/// downloading what's missing (curl, atomic per file — the NLI pattern
/// generalized). Returns the model directory.
pub(crate) fn provision(
    role: engram_core::cortex::Role,
    spec: &engram_core::cortex::ModelSpec,
) -> anyhow::Result<std::path::PathBuf> {
    let dir = engram_core::cortex::cache_dir(&spec.name)
        .context("no home directory for the model cache")?;
    let files = engram_core::cortex::spec_files(role, spec);
    if files.iter().all(|(name, _)| dir.join(name).is_file()) {
        return Ok(dir);
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    tracing::info!("downloading {} (one-time)…", spec.name);
    for (name, url) in files {
        let target = dir.join(&name);
        if target.is_file() {
            continue;
        }
        let part = dir.join(format!("{name}.part"));
        let ok = std::process::Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(&part)
            .arg(&url)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok || std::fs::rename(&part, &target).is_err() {
            let _ = std::fs::remove_file(&part);
            anyhow::bail!("downloading {url} failed");
        }
    }
    Ok(dir)
}

/// The pane's model admin: resolve a `/models` request, provision and load it,
/// swap it into the live set and every open engine, persist it, and re-embed
/// open stores for an embedder change (others re-embed on their next open).
pub(crate) struct CortexAdmin {
    pub(crate) models: Models,
    pub(crate) hub: Arc<Hub>,
    /// The core's lazy model slots: a hot-swap wraps the fresh model in a
    /// lazy handle and installs it as the slot the idle checker unloads (so
    /// unload keeps its chokepoint after a swap), and counts as model use —
    /// which is what makes `POST /models` while idle-unloaded just work.
    pub(crate) lazy: lazy::LazyModels,
    pub(crate) idle: Arc<engram_http::IdleTracker>,
}

impl engram_http::ModelAdmin for CortexAdmin {
    fn describe(&self) -> serde_json::Value {
        use engram_core::cortex::{self, Role};
        let cfg = cortex::load();
        let role_json = |role: Role| {
            let presets = cortex::presets(role);
            let custom = cfg
                .get(role)
                .filter(|s| !presets.iter().any(|p| p.name == s.name));
            serde_json::json!({
                "role": role.as_str(),
                "active": cfg.effective(role).name,
                "default": presets[0].name,
                "presets": presets,
                "custom": custom,
            })
        };
        serde_json::json!({
            "available": true,
            "fake_embeddings": self.models.current().embedder.is_fake(),
            "roles": [
                role_json(Role::Embedding),
                role_json(Role::Reranker),
                role_json(Role::Nli),
            ],
        })
    }

    fn apply(&self, request: serde_json::Value) -> engram_core::Result<serde_json::Value> {
        use engram_core::cortex::{self, Role};
        let role = Role::parse(request["role"].as_str().unwrap_or_default())?;
        let spec: cortex::ModelSpec = if let Some(name) = request["preset"].as_str() {
            cortex::presets(role)
                .into_iter()
                .find(|p| p.name == name)
                .ok_or_else(|| {
                    engram_core::Error::Project(format!(
                        "unknown preset {name:?} for role {}",
                        role.as_str()
                    ))
                })?
        } else if request["custom"].is_object() {
            serde_json::from_value(request["custom"].clone())?
        } else {
            return Err(engram_core::Error::Project(
                "pass either \"preset\" or \"custom\"".into(),
            ));
        };
        if role == Role::Embedding {
            if self.models.current().embedder.is_fake() {
                return Err(engram_core::Error::Project(
                    "this daemon runs --fake-embeddings — restart with real embeddings before switching models".into(),
                ));
            }
            if spec.dim.is_none() {
                return Err(engram_core::Error::Project(
                    "an embedding model needs \"dim\" (its vector width)".into(),
                ));
            }
        }
        let load_err = |e: anyhow::Error| engram_core::Error::Io(format!("{e:#}"));
        let mut reembedded = 0usize;
        // Every fresh model rides in a lazy wrapper so idle unload keeps its
        // chokepoint after a swap; loading it eagerly here is model use, so
        // the swap also ends any idle-unloaded state (see mark_loaded below).
        match role {
            Role::Embedding => {
                let embedder = self
                    .lazy
                    .swap_embedder(load_embedder(&spec).map_err(load_err)?);
                self.models.set.writing().embedder = embedder.clone();
                for engine in self.hub.engines() {
                    let mut engine = engine.locked();
                    engine.set_embedder(Box::new(embedder.clone()));
                    reembedded += engine.ensure_embed_model()?;
                }
            }
            Role::Reranker => {
                let reranker = self
                    .lazy
                    .swap_reranker(load_reranker(&spec).map_err(load_err)?);
                self.models.set.writing().reranker = Some(reranker.clone() as Arc<dyn Reranker>);
                for engine in self.hub.engines() {
                    engine.locked().set_reranker(Box::new(reranker.clone()));
                }
            }
            Role::Nli => {
                let nli = self.lazy.swap_nli(load_nli(&spec).map_err(load_err)?);
                self.models.set.writing().nli = Some(nli.clone() as Arc<dyn Nli>);
                for engine in self.hub.engines() {
                    engine.locked().set_nli(Box::new(nli.clone()));
                }
            }
        }
        let now = engram_core::now();
        self.idle.mark_loaded(now);
        self.idle.touch_model_use();
        // Persist: a default selection is the absence of config.
        let mut cfg = cortex::load();
        cfg.set(role, (!is_default_spec(role, &spec)).then(|| spec.clone()));
        cortex::save(&cfg)?;
        Ok(serde_json::json!({
            "role": role.as_str(),
            "applied": spec.name,
            "reembedded_nodes": reembedded,
        }))
    }
}

/// Best-effort download of the default NLI model (curl, exactly like
/// self-update; direct from Hugging Face — the user's chosen distribution).
/// Atomic per file (.part + rename); any failure leaves the cortex hint-less,
/// never blocks startup.
pub(crate) fn ensure_nli_model() {
    let Some(dir) = engram_core::nli::nli_model_dir() else {
        return;
    };
    if engram_core::nli::NLI_MODEL_FILES
        .iter()
        .all(|f| dir.join(f).is_file())
    {
        return;
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    const BASE: &str =
        "https://huggingface.co/techtheist/deberta-v3-small-tasksource-nli-onnx/resolve/main";
    let sources = [
        ("model.onnx", format!("{BASE}/onnx/model_quantized.onnx")),
        ("tokenizer.json", format!("{BASE}/tokenizer.json")),
        ("config.json", format!("{BASE}/config.json")),
    ];
    tracing::info!("downloading the NLI model (one-time, ~172 MB)…");
    for (name, url) in sources {
        let target = dir.join(name);
        if target.is_file() {
            continue;
        }
        let part = dir.join(format!("{name}.part"));
        let ok = std::process::Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(&part)
            .arg(&url)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok || std::fs::rename(&part, &target).is_err() {
            let _ = std::fs::remove_file(&part);
            tracing::warn!("NLI model download failed ({url}); cortex hints stay off");
            return;
        }
    }
}
