//! Embedding upkeep: node texts, the embed cache, model and composition changes.

use super::*;

impl Engine {
    pub(super) fn embed_node(&self, node: &Node) -> Result<()> {
        self.embed_node_with(node, None)
    }

    /// [`Engine::embed_node`], taking vectors a write computed before the
    /// lock wherever their texts match.
    pub(super) fn embed_node_with(&self, node: &Node, cache: Option<&EmbedCache>) -> Result<()> {
        let texts = node_texts(
            &self.store.config(),
            &node.title,
            node.body.as_deref(),
            &node.tags,
            &node.code_refs,
            node.fields.as_ref(),
        );
        let vectors = self.embed_texts(&texts, cache)?;
        self.store.upsert_embeddings(&node.id, &vectors)
    }

    /// Embed `texts`, taking each from `cache` when it holds exactly that
    /// text under the ACTIVE model — anything else (a model swapped since
    /// the cache was filled, a text the prediction missed) is embedded now.
    /// A cache can only make a write faster, never different.
    pub(crate) fn embed_texts(
        &self,
        texts: &[String],
        cache: Option<&EmbedCache>,
    ) -> Result<Vec<Vec<f32>>> {
        let Some(cache) = cache.filter(|c| c.model == self.embed_model_id()) else {
            return self.embedder.embed(texts);
        };
        let mut missing: Vec<String> = Vec::new();
        for t in texts {
            if !cache.vecs.contains_key(t) && !missing.contains(t) {
                missing.push(t.clone());
            }
        }
        let fresh: std::collections::HashMap<String, Vec<f32>> = if missing.is_empty() {
            Default::default()
        } else {
            let vecs = self.embedder.embed(&missing)?;
            missing.into_iter().zip(vecs).collect()
        };
        Ok(texts
            .iter()
            .map(|t| {
                cache
                    .vecs
                    .get(t)
                    .or_else(|| fresh.get(t))
                    .cloned()
                    .expect("every text was cached or embedded")
            })
            .collect())
    }

    pub(super) fn embed_one_with(
        &self,
        text: &str,
        cache: Option<&EmbedCache>,
    ) -> Result<Vec<f32>> {
        Ok(self
            .embed_texts(&[text.to_string()], cache)?
            .pop()
            .expect("one vector for one text"))
    }

    /// Bring stored vectors in line with the ACTIVE embedding model (PLAN §7A
    /// model selection), returning how many nodes were re-embedded. A store
    /// records the identity its vectors were computed with; when the active
    /// model differs — different name or width — vector storage is rebuilt
    /// for the new width and the whole graph re-embeds. Skipped entirely
    /// under a fake embedder (fake vectors must never replace real ones), so
    /// a `--fake-embeddings` open can never mass-destroy a graph's vectors.
    pub fn ensure_embed_model(&self) -> Result<usize> {
        if self.embedder.is_fake() {
            return Ok(0);
        }
        let active = self.embed_model_id();
        let stored = self.store.embed_model()?;
        // Stores that predate model selection carry no identity: they are the
        // default model by construction — stamp, don't re-embed.
        let effective = stored.clone().unwrap_or(EmbedModelId {
            name: crate::rag::DEFAULT_EMBED_MODEL.to_string(),
            dim: crate::rag::EMBED_DIM,
        });
        if effective == active {
            if stored.is_none() {
                self.store.set_embed_model(&active)?;
            }
            // Same identity, but a swap that died mid-loop may have left
            // gaps — backfill any node without a vector so every open heals.
            let mut healed = 0;
            for n in self.store.all_nodes()? {
                if self.store.embedding_of(&n.id)?.is_none() {
                    self.embed_node(&n)?;
                    healed += 1;
                }
            }
            return Ok(healed);
        }
        self.store.reset_vectors(active.dim)?;
        // Record the new identity BEFORE the loop: the TepinDB backend stamps
        // each written vector with the store's recorded model, and the file
        // pins itself to whatever the first write says — stamping after the
        // loop would pin the file under the OLD name and poison every later
        // write with embedder_mismatch (bit us live on the first real swap).
        self.store.set_embed_model(&active)?;
        let nodes = self.store.all_nodes()?;
        for n in &nodes {
            self.embed_node(n)?;
        }
        // A full re-embed is by definition the current composition too.
        self.store.set_embed_version(EMBED_COMPOSITION)?;
        Ok(nodes.len())
    }

    /// Bring stored vectors up to the current [`EMBED_COMPOSITION`], returning
    /// how many nodes were re-embedded (0 = already current or skipped).
    /// Skipped with a fake embedder over a non-empty graph — fake vectors must
    /// never replace real ones, and the brief hook routinely opens real DBs
    /// with `--fake-embeddings`. Idempotent; stamps the version when done.
    pub fn ensure_embed_composition(&self) -> Result<usize> {
        if self.store.embed_version()? >= EMBED_COMPOSITION {
            return Ok(0);
        }
        let nodes = self.store.all_nodes()?;
        if self.embedder.is_fake() && !nodes.is_empty() {
            return Ok(0);
        }
        // The composition change also reshapes the vector layout (claim
        // chunks since v3) — clear storage once, then rebuild it whole.
        self.store.reset_vectors(self.embedder.dim())?;
        for n in &nodes {
            self.embed_node(n)?;
        }
        self.store.set_embed_version(EMBED_COMPOSITION)?;
        Ok(nodes.len())
    }
}

/// Sentence-sized claim texts for retrieval (v0.6.3 claim-level search):
/// the tokenizer splits the body on sentence boundaries and keeps units
/// substantial enough to mean something alone — each becomes its own vector
/// next to the node-level one, mirroring how the NLI layer already judges
/// claims instead of whole bodies. Title rides along with each claim so a
/// sentence keeps its subject.
pub(crate) fn claim_texts(title: &str, body: Option<&str>) -> Vec<String> {
    const MIN_CHARS: usize = 30;
    const MAX_CLAIMS: usize = 12;
    let Some(body) = body.filter(|b| b.trim().len() >= MIN_CHARS) else {
        return Vec::new();
    };
    let mut claims = Vec::new();
    let mut current = String::new();
    let flat = body.replace('\n', " ");
    let mut chars = flat.chars().peekable();
    while let Some(c) = chars.next() {
        current.push(c);
        let boundary =
            matches!(c, '.' | '!' | '?' | ';') && chars.peek().is_none_or(|n| n.is_whitespace());
        if boundary || chars.peek().is_none() {
            let sentence = current.trim();
            if sentence.len() >= MIN_CHARS {
                claims.push(format!("{title}. {sentence}"));
                if claims.len() == MAX_CLAIMS {
                    break;
                }
            }
            current.clear();
        }
    }
    // A single claim adds nothing over the composition vector.
    if claims.len() <= 1 {
        Vec::new()
    } else {
        claims
    }
}

/// Which embedding composition stored vectors were computed with (kept in
/// `PRAGMA user_version`). Bump when [`embed_text`] changes what it includes;
/// [`Engine::ensure_embed_composition`] re-embeds databases that are behind.
/// 0 = legacy title+body; 2 = full fields (title, body, tags, code_refs);
/// 3 = claim-chunked (the node-level vector plus one vector per body
/// sentence, so a query matching one claim in a rich body finds the node).
pub const EMBED_COMPOSITION: i64 = 3;

/// Every text a node is embedded as, in storage order: the node-level
/// composition (with the graph's indexed custom fields, 0.9.0) first, then
/// one text per claim sentence.
fn node_texts(
    cfg: &crate::config::GraphConfig,
    title: &str,
    body: Option<&str>,
    tags: &[String],
    code_refs: &[String],
    fields: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Vec<String> {
    let mut composed = embed_text(title, body, tags, code_refs);
    let fields_text = fields_embed_text(cfg, fields);
    if !fields_text.is_empty() {
        composed.push('\n');
        composed.push_str(&fields_text);
    }
    let mut texts = vec![composed];
    texts.extend(claim_texts(title, body));
    texts
}

/// Vectors a checked write computed BEFORE taking the engine lock (0.9.11,
/// Insight 00dxfvuv1j1f: ~144 of the ~175 ms a write held the lock at 1500
/// notes was embedding). The write predicts every text the store and its
/// checks will embed — the same scrub and tag normalization the store
/// applies — and embeds them with the lock released; under the lock
/// [`Engine::embed_texts`] takes a vector only for a byte-identical text
/// under the same model, and embeds anything else on the spot. A wrong
/// prediction, a model swapped mid-write or a field index changed meanwhile
/// cost a little time, never a different vector.
pub(crate) struct EmbedCache {
    pub(super) model: EmbedModelId,
    pub(super) vecs: std::collections::HashMap<String, Vec<f32>>,
}

impl EmbedCache {
    /// Embed `texts` (duplicates once). `None` when embedding fails — the
    /// write then embeds under the lock and reports the error there.
    fn fill(embedder: &dyn Embedder, model: EmbedModelId, texts: Vec<String>) -> Option<Self> {
        let mut unique: Vec<String> = Vec::with_capacity(texts.len());
        for t in texts {
            if !unique.contains(&t) {
                unique.push(t);
            }
        }
        let vecs = embedder.embed(&unique).ok()?;
        (vecs.len() == unique.len()).then(|| Self {
            model,
            vecs: unique.into_iter().zip(vecs).collect(),
        })
    }

    /// What a checked add will embed: the duplicate check's text, then the
    /// stored node's texts as the store will shape the node.
    pub(crate) fn for_add(
        embedder: &dyn Embedder,
        model: EmbedModelId,
        cfg: &crate::config::GraphConfig,
        n: &NewNode,
    ) -> Option<Self> {
        let title = crate::redact::scrub(&n.title);
        let body = n.body.as_deref().map(crate::redact::scrub);
        let fields = n
            .fields
            .as_ref()
            .filter(|f| !f.is_empty())
            .map(crate::redact::scrub_fields);
        let mut texts = vec![embed_text(&title, body.as_deref(), &n.tags, &n.code_refs)];
        texts.extend(node_texts(
            cfg,
            &title,
            body.as_deref(),
            &crate::store::normalize_tags(&n.tags),
            &n.code_refs,
            fields.as_ref(),
        ));
        Self::fill(embedder, model, texts)
    }

    /// What a checked update will embed, read off the stored node as the
    /// patch will leave it. `None` when the patch touches no embedded text.
    pub(crate) fn for_update(
        embedder: &dyn Embedder,
        model: EmbedModelId,
        cfg: &crate::config::GraphConfig,
        before: &Node,
        patch: &NodePatch,
    ) -> Option<Self> {
        let touches_text = patch.title.is_some()
            || patch.body.is_some()
            || patch.tags.is_some()
            || patch.code_refs.is_some()
            || patch.fields.is_some();
        if !touches_text {
            return None;
        }
        let title = patch
            .title
            .as_deref()
            .map_or_else(|| before.title.clone(), crate::redact::scrub);
        let body = match &patch.body {
            Some(b) => Some(crate::redact::scrub(b)),
            None => before.body.clone(),
        };
        let tags = match &patch.tags {
            Some(t) => crate::store::normalize_tags(t),
            None => before.tags.clone(),
        };
        let code_refs = patch.code_refs.as_ref().unwrap_or(&before.code_refs);
        let fields = match &patch.fields {
            Some(p) => {
                let resolved = Engine::resolve_fields_patch(before.fields.as_ref(), p);
                (!resolved.is_empty()).then(|| crate::redact::scrub_fields(&resolved))
            }
            None => before.fields.clone(),
        };
        let mut texts = vec![embed_text(&title, body.as_deref(), &tags, code_refs)];
        texts.extend(node_texts(
            cfg,
            &title,
            body.as_deref(),
            &tags,
            code_refs,
            fields.as_ref(),
        ));
        Self::fill(embedder, model, texts)
    }
}

/// The text a node is embedded as — kept in one place so write-time similarity
/// checks embed exactly what storage embeds. Tags and code_refs ride along so
/// "everything about policy.rs" works as a semantic query, not only a keyword
/// one; title+body still dominate the vector, so dupe detection is unaffected.
pub(super) fn embed_text(
    title: &str,
    body: Option<&str>,
    tags: &[String],
    code_refs: &[String],
) -> String {
    let mut text = title.to_string();
    if let Some(b) = body.filter(|b| !b.is_empty()) {
        text.push('\n');
        text.push_str(b);
    }
    if !tags.is_empty() {
        text.push('\n');
        text.push_str(&tags.join(" "));
    }
    if !code_refs.is_empty() {
        text.push('\n');
        text.push_str(&code_refs.join(" "));
    }
    text
}

/// The `name: value` lines a node's INDEXED custom fields contribute to its
/// embedding. Per-graph, so it rides beside [`embed_text`] rather
/// than inside it: the global [`EMBED_COMPOSITION`] stays honest for graphs
/// without indexed fields, and the per-graph fingerprint guard
/// ([`Engine::ensure_field_index`]) re-embeds when a graph's set changes.
fn fields_embed_text(
    cfg: &crate::config::GraphConfig,
    fields: Option<&serde_json::Map<String, serde_json::Value>>,
) -> String {
    let Some(fields) = fields else {
        return String::new();
    };
    let mut out = String::new();
    for f in cfg.fields.iter().filter(|f| f.indexed) {
        if let Some(v) = fields.get(&f.name) {
            let text = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("{}: {}", f.name, text));
        }
    }
    out
}
