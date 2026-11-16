//! Reads across projects: the roster, search and claim checks over every graph, and the brief.

use super::*;

impl Hub {
    /// Every project this hub can reach: current, home, then the registry.
    pub fn projects(&self) -> Vec<ProjectInfo> {
        self.projects_for(None)
    }

    /// The roster as a SESSION sees it: `bound` names the project this caller
    /// is bound to (an MCP session on `/projects/{id}/mcp`), `None` = the
    /// process's launch project. Only the `current` flag moves — a bound
    /// session must never be told it is somewhere else, which since the
    /// home-rooted core role is what every bound session was told.
    pub fn projects_for(&self, bound: Option<&str>) -> Vec<ProjectInfo> {
        let current_id = bound
            .and_then(|sel| self.resolve_id(sel).ok())
            .unwrap_or_else(|| self.current.id.clone());
        let mut out = self.projects_inner();
        for row in &mut out {
            row.current = row.id == current_id;
        }
        out
    }

    fn projects_inner(&self) -> Vec<ProjectInfo> {
        let open = self.open.locked();
        // A core launched outside any project has home AS its current
        // project (v0.6.2 serve-anywhere) — one row, both hats.
        let home_is_current = self.current.id == HOME_PROJECT;
        let mut out = vec![ProjectInfo {
            id: self.current.id.clone(),
            name: self.current.name.clone(),
            root: self.current.root.as_ref().map(|p| p.display().to_string()),
            db: self
                .current
                .db
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            current: true,
            home: home_is_current,
            open: true,
            last_seen: None,
        }];
        if let Some(db) = registry::home_db_path().filter(|_| !home_is_current) {
            out.push(ProjectInfo {
                id: HOME_PROJECT.into(),
                name: HOME_PROJECT.into(),
                root: None,
                db: db.display().to_string(),
                current: false,
                home: true,
                open: open.contains_key(HOME_PROJECT),
                last_seen: None,
            });
        }
        for p in registry::load().projects {
            if self.entry_is_current(&p) {
                continue;
            }
            out.push(ProjectInfo {
                open: open.contains_key(&p.id),
                db: p.resolved_db().display().to_string(),
                id: p.id,
                name: p.name,
                root: Some(p.root),
                current: false,
                home: false,
                last_seen: Some(p.last_seen),
            });
        }
        out
    }

    pub(super) fn entry_is_current(&self, p: &ProjectEntry) -> bool {
        p.id == self.current.id
            || self
                .current
                .root
                .as_ref()
                .is_some_and(|r| Path::new(&p.root) == r)
    }

    /// Sibling projects (registry minus current), opened lazily; unopenable
    /// ones are reported, never silently dropped.
    fn others(&self, include_home: bool) -> (NamedEngines, Vec<String>) {
        let mut engines = Vec::new();
        let mut skipped = Vec::new();
        // Home as the launch graph is already `current`, read by the caller.
        if include_home
            && self.current.id != HOME_PROJECT
            && registry::home_db_path().is_some_and(|p| p.is_file())
        {
            match self.open_home() {
                Ok(e) => engines.push((HOME_PROJECT.to_string(), e)),
                Err(err) => skipped.push(format!("{HOME_PROJECT}: {err}")),
            }
        }
        for entry in registry::load().projects {
            if self.entry_is_current(&entry) {
                continue;
            }
            let db = entry.resolved_db();
            if !db.is_file() {
                skipped.push(format!("{}: db missing ({})", entry.name, db.display()));
                continue;
            }
            match self.open_entry(&entry) {
                Ok(e) => engines.push((entry.name, e)),
                Err(err) => skipped.push(format!("{}: {err}", entry.name)),
            }
        }
        (engines, skipped)
    }

    /// `project: "all"` — search the current project at full weight, then
    /// every sibling and the home graph under the locality prior, provenance
    /// on every foreign hit. Engines are locked one at a time.
    pub fn search_all(
        &self,
        query: &str,
        types: &[NodeType],
        limit: usize,
    ) -> Result<(Vec<SearchHit>, Vec<String>)> {
        let mut hits = self.current.engine.locked().search(query, types, limit)?;
        let (others, mut skipped) = self.others(true);
        for (name, engine) in others {
            match engine.locked().search(query, types, limit) {
                Ok(mut foreign) => {
                    for hit in &mut foreign {
                        hit.score *= crate::policy::CROSS_PROJECT_PRIOR;
                        hit.project = Some(name.clone());
                    }
                    hits.extend(foreign);
                }
                Err(e) => skipped.push(format!("{name}: {e}")),
            }
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(limit);
        Ok((hits, skipped))
    }

    /// `check_claim` across every reachable graph, buckets merged with
    /// provenance. The current project judges first; a sibling without the
    /// NLI layer is reported in `skipped`.
    pub fn check_claim_all(&self, text: &str, limit: usize) -> Result<(ClaimReport, Vec<String>)> {
        let mut report = self.current.engine.locked().check_claim(text, limit)?;
        let (others, mut skipped) = self.others(true);
        for (name, engine) in others {
            match engine.locked().check_claim(text, limit) {
                Ok(foreign) => {
                    let tag = |mut vs: Vec<ClaimVerdict>| {
                        for v in &mut vs {
                            v.project = Some(name.clone());
                        }
                        vs
                    };
                    report.supports.extend(tag(foreign.supports));
                    report.contradicts.extend(tag(foreign.contradicts));
                    report.retracted.extend(tag(foreign.retracted));
                    report.silent.extend(tag(foreign.silent));
                }
                Err(e) => skipped.push(format!("{name}: {e}")),
            }
        }
        report
            .contradicts
            .sort_by(|a, b| b.contradiction.total_cmp(&a.contradiction));
        report
            .supports
            .sort_by(|a, b| b.entailment.total_cmp(&a.entailment));
        Ok((report, skipped))
    }

    /// The session brief: the current project's digest, a capped section of
    /// home-graph canon, and one line naming the other reachable graphs. The
    /// extras ride inside the same character budget so the total never
    /// exceeds what the caller asked for; with no siblings and no home graph
    /// they cost nothing.
    pub fn brief(&self, max_chars: usize) -> Result<String> {
        self.brief_for(None, max_chars)
    }

    /// The brief a SESSION sees: `bound` names the project this caller is
    /// bound to (an MCP session on `/projects/{id}/mcp`), `None` = the
    /// process's launch project. The digest, its budget, the home section and
    /// the roster are all computed against THAT project — reading
    /// `self.current` instead used to be invisibly right (the daemon launched
    /// in the repo it served) and became always-wrong when the machine core
    /// became a dedicated home-rooted process.
    pub fn brief_for(&self, bound: Option<&str>, max_chars: usize) -> Result<String> {
        let engine = match bound {
            Some(sel) => self.get(sel)?,
            None => self.current.engine.clone(),
        };
        let id = match bound {
            Some(sel) => self.resolve_id(sel)?,
            None => self.current.id.clone(),
        };
        let reserve = engine.locked().config().brief.home_reserve;
        // Briefing the home graph itself: the ride-along section would be the
        // same canon twice.
        let home = (id != HOME_PROJECT)
            .then(|| self.home_brief_section(reserve))
            .flatten();
        let roster = self.projects_brief_section(&id);
        let reserve = home.as_deref().map_or(0, str::len) + roster.as_deref().map_or(0, str::len);
        let mut out = engine.locked().brief(max_chars.saturating_sub(reserve))?;
        // The first line names the graph (issue #11): an agent verifying
        // its binding after a resume reads one line, not the roster.
        let header = if id == HOME_PROJECT {
            "# Engram brief — the home graph (user-level, no project)".to_string()
        } else {
            let root = self.project_root(&id);
            match root {
                Some(root) => format!(
                    "# Engram brief — project '{}' ({root})",
                    self.project_name(&id)
                ),
                None => format!("# Engram brief — project '{}'", self.project_name(&id)),
            }
        };
        // Installed instructions the core's upkeep held back because git
        // tracks them: the agent is the one who can tell the user.
        let header = match self
            .project_root(&id)
            .and_then(|root| crate::setup_record::brief_notice(std::path::Path::new(&root)))
        {
            Some(notice) => format!("{header}\n{notice}"),
            None => header,
        };
        if let Some(rest) = out.strip_prefix("# Engram brief\n") {
            out = format!("{header}\n{rest}");
        }
        if let Some(section) = home {
            out.push_str(&section);
        }
        if let Some(section) = roster {
            out.push_str(&section);
        }
        Ok(out)
    }

    /// The brief's project roster: which other graphs
    /// this session can reach and how. Emitted only when cross-project access
    /// actually works here (a factory is present) and something is reachable —
    /// advertising graphs a single-project instance can't open would mislead.
    fn projects_brief_section(&self, bound_id: &str) -> Option<String> {
        self.factory.as_ref()?;
        // "Other" is relative to the BRIEFED project, not the launch one: a
        // session bound to X must not be offered X as somewhere else to look.
        let bound_is_launch = bound_id == self.current.id;
        let mut names: Vec<String> = registry::load()
            .projects
            .iter()
            .filter(|p| p.id != bound_id && !(bound_is_launch && self.entry_is_current(p)))
            .map(|p| p.name.clone())
            .collect();
        names.sort();
        if bound_id != HOME_PROJECT && registry::home_db_path().is_some_and(|p| p.is_file()) {
            names.push(HOME_PROJECT.into());
        }
        if names.is_empty() {
            return None;
        }
        Some(format!(
            "\n## Other project graphs on this machine\nReachable via the `project` argument \
             most tools accept: {} — or `project: \"all\"` on search/check_claim to read across \
             everything (foreign hits carry provenance). Capture knowledge about a sibling \
             project into ITS graph; `home` holds user-level canon. `list_projects` has details.\n",
            names.join(", ")
        ))
    }

    /// The brief's home-graph section: top user-level principles and cautions,
    /// best-effort — never opens (and thus never creates) a home graph that
    /// doesn't exist yet, and any failure just drops the section.
    fn home_brief_section(&self, max_chars: usize) -> Option<String> {
        if !registry::home_db_path()?.is_file() {
            return None;
        }
        let engine = self.open_home().ok()?;
        let engine = engine.locked();
        let mut out = String::new();
        // The home graph's own canon sections, scaled to a ride-along sample:
        // strongest rank priors first (Principles/Cautions before Decisions
        // in the shipped set), each at about a third of its full brief cap.
        let cfg = engine.config();
        let mut sections: Vec<_> = cfg.ontology.types.iter().filter(|t| t.brief.show).collect();
        sections.sort_by(|a, b| b.roles.rank_prior.total_cmp(&a.roles.rank_prior));
        for t in sections {
            let node_type = NodeType::parse(&t.name).ok()?;
            let cap = t.brief.cap.div_ceil(3).max(2);
            let nodes = engine.store().nodes_by_type_active(&node_type, cap).ok()?;
            for n in nodes {
                let line = crate::engine::node_line(&n, crate::engine::EXCERPT_CHARS);
                if out.len() + line.len() + 1 > max_chars.saturating_sub(HOME_HEADING.len()) {
                    break;
                }
                out.push_str(&line);
                out.push('\n');
            }
        }
        if out.is_empty() {
            return None;
        }
        Some(format!("{HOME_HEADING}{out}"))
    }

    /// Promotion nominations: current-project Principles/Cautions
    /// that recur (same type, high similarity) in other projects' graphs and
    /// aren't already represented in the home graph. Read-only — the user
    /// promotes from the pane; nothing self-applies.
    pub fn promotion_candidates(&self) -> Result<(Vec<PromotionCandidate>, Vec<String>)> {
        // Snapshot the current project's candidates with their vectors first,
        // then release its lock before touching any sibling engine.
        let mut seeds: Vec<(Node, Vec<f32>)> = Vec::new();
        {
            // Promotion stays keyed on the shipped canon names for now:
            // cross-ontology role mapping is a tracked §7D risk — a custom
            // ontology without these types simply yields no candidates.
            let engine = self.current.engine.locked();
            for node_type in [NodeType::Principle, NodeType::Caution] {
                let total = engine.store().count_by_type_active(&node_type)? as usize;
                for node in engine.store().nodes_by_type_active(&node_type, total)? {
                    if let Some(vec) = engine.store().embedding_of(&node.id)? {
                        seeds.push((node, vec));
                    }
                }
            }
        }
        let (others, skipped) = self.others(false);
        let home = registry::home_db_path()
            .filter(|p| p.is_file())
            .and_then(|_| self.open_home().ok());

        let mut candidates = Vec::new();
        for (node, vec) in seeds {
            // Already promoted? A same-type near-duplicate in the home graph
            // means this knowledge is user-level already.
            if let Some(home) = &home
                && !similar_same_type(&home.locked(), &vec, node.node_type.clone())?.is_empty()
            {
                continue;
            }
            let mut matches = Vec::new();
            for (name, engine) in &others {
                for (id, title, similarity) in
                    similar_same_type(&engine.locked(), &vec, node.node_type.clone())?
                {
                    matches.push(PromotionMatch {
                        project: name.clone(),
                        id,
                        title,
                        similarity,
                    });
                }
            }
            if !matches.is_empty() {
                matches.sort_by(|a, b| b.similarity.total_cmp(&a.similarity));
                candidates.push(PromotionCandidate { node, matches });
            }
        }
        candidates.sort_by(|a, b| {
            let best = |c: &PromotionCandidate| c.matches.first().map_or(0.0, |m| m.similarity);
            best(b).total_cmp(&best(a))
        });
        Ok((candidates, skipped))
    }
}

const HOME_HEADING: &str = "\n## Home graph (user-level canon, shared across projects)\n";

/// Active same-type nodes in `engine`'s store within the promotion band.
fn similar_same_type(
    engine: &Engine,
    vec: &[f32],
    node_type: NodeType,
) -> Result<Vec<(String, String, f64)>> {
    let mut out = Vec::new();
    for (id, distance) in engine.store().search_vec(vec, 5)? {
        let similarity = 1.0 - distance;
        if similarity < crate::policy::PROMOTION_SIMILARITY {
            break; // distance-ordered
        }
        if let Some(n) = engine.store().get_node(&id)?
            && n.node_type == node_type
            && n.valid_until.is_none()
        {
            out.push((n.id, n.title, similarity));
        }
    }
    Ok(out)
}
