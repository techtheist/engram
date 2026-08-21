//! Reload-capable model slots for the machine core's idle ONNX unload
//! (strict-spec Decision: unload all three sessions after 15 idle minutes,
//! keep the core resident, reload lazily and single-flight on first demand).
//!
//! The shape: at core startup every loaded model is wrapped in a lazy handle
//! that holds `Option<loaded>` plus a loader closure that can rebuild it from
//! the machine-level selection (`~/.engram/models.json`, read at reload time
//! so a hot-swap persisted while unloaded is honored). The wrappers are
//! pushed into every open engine over the same handle-push path model
//! hot-swap uses, and written back into the live [`crate::Models`] set so
//! engines opened later get them too. Unload clears the inner `Option` —
//! every holder shares the same `Arc`, so the sessions drop as soon as any
//! in-flight call finishes. Reload happens inside the wrapper under its
//! write lock, which is the single-flight guarantee: concurrent callers
//! block on the lock and find the freshly loaded model.
//!
//! Every use stamps the idle tracker's model-use clock — the chokepoint the
//! "never unload mid-sweep" guard relies on: harvester embeds and librarian
//! NLI sweeps extend the clock exactly like search does, without counting as
//! HTTP activity.
//!
//! Fake embeddings are wrapped too (the tests exercise the full
//! unload/reload machinery through them): their loader just rebuilds a
//! `FakeEmbedder`, so the "reload" is trivial and dependency-free.

use std::sync::{Arc, RwLock};

use engram_core::{Embedder, FakeEmbedder, Nli, Reranker};
use engram_http::IdleTracker;

type Loader<T> = Box<dyn Fn() -> anyhow::Result<Arc<T>> + Send + Sync>;

/// One reload-capable slot: the loaded model (or nothing), how to rebuild
/// it, and the tracker its uses stamp.
struct Slot<T: ?Sized> {
    inner: RwLock<Option<Arc<T>>>,
    loader: Loader<T>,
    role: &'static str,
    idle: Arc<IdleTracker>,
}

impl<T: ?Sized> Slot<T> {
    fn preloaded(
        model: Arc<T>,
        loader: Loader<T>,
        role: &'static str,
        idle: Arc<IdleTracker>,
    ) -> Self {
        Self {
            inner: RwLock::new(Some(model)),
            loader,
            role,
            idle,
        }
    }

    /// The single-flight reload point: the write lock is held for the whole
    /// load, so exactly one caller pays it and everyone else blocks briefly
    /// and finds the loaded model.
    fn get(&self) -> engram_core::Result<Arc<T>> {
        self.idle.touch_model_use();
        if let Some(model) = self.inner.read().expect("model slot lock").clone() {
            return Ok(model);
        }
        let mut slot = self.inner.write().expect("model slot lock");
        if let Some(model) = slot.clone() {
            // Another caller reloaded while we waited on the lock.
            return Ok(model);
        }
        tracing::info!("reloading the {} model on demand", self.role);
        let started = std::time::Instant::now();
        let model = (self.loader)().map_err(|e| {
            engram_core::Error::Embedding(format!("reloading the {} model: {e:#}", self.role))
        })?;
        *slot = Some(model.clone());
        // Any single reload ends the idle-unloaded state — the remaining
        // slots reload on their own first use.
        self.idle.mark_loaded(engram_core::now());
        self.idle.touch_model_use();
        tracing::info!(
            "{} model reloaded in {:.1}s",
            self.role,
            started.elapsed().as_secs_f32()
        );
        Ok(model)
    }

    /// Drop the session. Returns whether one was actually resident.
    fn unload(&self) -> bool {
        self.inner
            .write()
            .expect("model slot lock")
            .take()
            .is_some()
    }
}

/// Lazy embedder handle. Identity metadata (name/dim/fakeness) is cached at
/// wrap time so `ensure_embed_model` checks and `/models` describes never
/// force a load; a hot-swap builds a NEW wrapper (with the new identity) and
/// pushes it, exactly like the pre-unload swap pushed raw handles.
pub struct LazyEmbedder {
    slot: Slot<dyn Embedder>,
    name: String,
    dim: usize,
    fake: bool,
}

impl LazyEmbedder {
    pub fn wrap(model: Arc<dyn Embedder>, idle: Arc<IdleTracker>) -> Arc<Self> {
        let fake = model.is_fake();
        let name = model.name().to_string();
        let dim = model.dim();
        let loader: Loader<dyn Embedder> = if fake {
            Box::new(|| Ok(Arc::new(FakeEmbedder::default()) as Arc<dyn Embedder>))
        } else {
            Box::new(|| {
                let cfg = engram_core::cortex::load();
                crate::load_embedder(&cfg.effective(engram_core::cortex::Role::Embedding))
            })
        };
        Arc::new(Self {
            slot: Slot::preloaded(model, loader, "embedding", idle),
            name,
            dim,
            fake,
        })
    }

    fn unload(&self) -> bool {
        self.slot.unload()
    }
}

impl Embedder for LazyEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn is_fake(&self) -> bool {
        self.fake
    }

    fn embed(&self, texts: &[String]) -> engram_core::Result<Vec<Vec<f32>>> {
        self.slot.get()?.embed(texts)
    }
}

pub struct LazyReranker {
    slot: Slot<dyn Reranker>,
}

impl LazyReranker {
    pub fn wrap(model: Arc<dyn Reranker>, idle: Arc<IdleTracker>) -> Arc<Self> {
        let loader: Loader<dyn Reranker> = Box::new(|| {
            let cfg = engram_core::cortex::load();
            crate::load_reranker(&cfg.effective(engram_core::cortex::Role::Reranker))
        });
        Arc::new(Self {
            slot: Slot::preloaded(model, loader, "reranker", idle),
        })
    }

    fn unload(&self) -> bool {
        self.slot.unload()
    }
}

impl Reranker for LazyReranker {
    fn rank(&self, query: &str, documents: &[String]) -> engram_core::Result<Vec<f32>> {
        self.slot.get()?.rank(query, documents)
    }
}

pub struct LazyNli {
    slot: Slot<dyn Nli>,
}

impl LazyNli {
    pub fn wrap(model: Arc<dyn Nli>, idle: Arc<IdleTracker>) -> Arc<Self> {
        let loader: Loader<dyn Nli> = Box::new(|| {
            let cfg = engram_core::cortex::load();
            crate::load_nli(&cfg.effective(engram_core::cortex::Role::Nli))
        });
        Arc::new(Self {
            slot: Slot::preloaded(model, loader, "NLI", idle),
        })
    }

    fn unload(&self) -> bool {
        self.slot.unload()
    }
}

impl Nli for LazyNli {
    fn judge(
        &self,
        pairs: &[(String, String)],
    ) -> engram_core::Result<Vec<engram_core::NliJudgment>> {
        self.slot.get()?.judge(pairs)
    }
}

/// The core's handle on its three lazy slots — what the idle checker unloads.
/// Internally shared (cloning shares the same slots), and hot-swap replaces a
/// slot's wrapper through [`LazyModels::swap_embedder`] and friends so the
/// idle checker always unloads the CURRENT wrappers, never stale ones.
/// Slots that never loaded (reranker/NLI under `--fake-embeddings`, or a
/// failed cortex load at startup) stay absent: unload has nothing to drop
/// there and reload never resurrects a layer the startup deliberately
/// degraded away.
#[derive(Clone)]
pub struct LazyModels {
    idle: Arc<IdleTracker>,
    inner: Arc<RwLock<Wrappers>>,
}

struct Wrappers {
    embedder: Arc<LazyEmbedder>,
    reranker: Option<Arc<LazyReranker>>,
    nli: Option<Arc<LazyNli>>,
}

impl LazyModels {
    /// Wrap the already-loaded model set: write the wrappers back into the
    /// live set (engines opened later read it) and push them into every
    /// engine the hub holds open — the same push path hot-swap uses. The
    /// embedder's identity is unchanged by wrapping, so no re-embed check is
    /// needed.
    pub fn install(
        models: &crate::Models,
        hub: &engram_core::Hub,
        idle: &Arc<IdleTracker>,
    ) -> Self {
        let current = models.current();
        let embedder = LazyEmbedder::wrap(current.embedder, idle.clone());
        let reranker = current
            .reranker
            .map(|r| LazyReranker::wrap(r, idle.clone()));
        let nli = current.nli.map(|n| LazyNli::wrap(n, idle.clone()));
        {
            let mut set = models.set.write().expect("model set lock");
            set.embedder = embedder.clone();
            set.reranker = reranker.clone().map(|r| r as Arc<dyn Reranker>);
            set.nli = nli.clone().map(|n| n as Arc<dyn Nli>);
        }
        for engine in hub.engines() {
            let mut engine = engine.lock().expect("engine lock");
            engine.set_embedder(Box::new(embedder.clone()));
            if let Some(r) = &reranker {
                engine.set_reranker(Box::new(r.clone()));
            }
            if let Some(n) = &nli {
                engine.set_nli(Box::new(n.clone()));
            }
        }
        Self {
            idle: idle.clone(),
            inner: Arc::new(RwLock::new(Wrappers {
                embedder,
                reranker,
                nli,
            })),
        }
    }

    /// Hot-swap: wrap a freshly loaded embedder and make it the slot the
    /// idle checker unloads. The caller pushes the returned wrapper into the
    /// live set and every open engine.
    pub fn swap_embedder(&self, model: Arc<dyn Embedder>) -> Arc<LazyEmbedder> {
        let wrapper = LazyEmbedder::wrap(model, self.idle.clone());
        self.inner.write().expect("lazy models lock").embedder = wrapper.clone();
        wrapper
    }

    pub fn swap_reranker(&self, model: Arc<dyn Reranker>) -> Arc<LazyReranker> {
        let wrapper = LazyReranker::wrap(model, self.idle.clone());
        self.inner.write().expect("lazy models lock").reranker = Some(wrapper.clone());
        wrapper
    }

    pub fn swap_nli(&self, model: Arc<dyn Nli>) -> Arc<LazyNli> {
        let wrapper = LazyNli::wrap(model, self.idle.clone());
        self.inner.write().expect("lazy models lock").nli = Some(wrapper.clone());
        wrapper
    }

    /// Drop every resident session. Returns whether anything was resident.
    pub fn unload_all(&self) -> bool {
        let wrappers = self.inner.read().expect("lazy models lock");
        let mut any = wrappers.embedder.unload();
        if let Some(r) = &wrappers.reranker {
            any |= r.unload();
        }
        if let Some(n) = &wrappers.nli {
            any |= n.unload();
        }
        any
    }
}
