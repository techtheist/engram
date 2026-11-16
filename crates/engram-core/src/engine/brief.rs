//! The session brief and the one-line node rendering it shares.

use super::*;

impl Engine {
    /// The session-start brief: a budgeted markdown digest of conflicts, suspects,
    /// recent changes, open work, then per-type canon. Composition comes from the
    /// graph's config. Every included node counts as reused (its decay clock
    /// refreshes).
    pub fn brief(&self, max_chars: usize) -> Result<String> {
        let cfg = self.store.config();
        let bc = &cfg.brief;
        let mut out = String::from("# Engram brief\n");
        let mut included: Vec<String> = Vec::new();
        // `brief.bodies` off = titles only, whatever each section's excerpt.
        let ex = |excerpt: usize| if bc.bodies { excerpt } else { 0 };
        let mut seen = std::collections::HashSet::new();

        let push_line = |out: &mut String, line: &str| -> bool {
            if out.len() + line.len() + 1 > max_chars {
                return false;
            }
            out.push_str(line);
            out.push('\n');
            true
        };

        'assemble: {
            // Teach the ontology up front when configured (custom ontologies
            // the assistant's skill can't know; off in the shipped preset —
            // `describe_ontology` serves the same content on demand).
            if bc.ontology.show {
                for line in cfg.describe_ontology().lines() {
                    if !push_line(&mut out, line) {
                        break 'assemble;
                    }
                }
            }

            // Version tracking: the current working version leads the brief
            // (every version-bound note is stamped with it; set_version
            // moves it when the project does).
            let working_version = if cfg.versioning.enabled {
                self.store.current_version()?
            } else {
                None
            };
            if cfg.versioning.enabled {
                let line = match working_version.clone() {
                    Some(v) => format!(
                        "Current working version: {v} — new notes are stamped with it, and brief lines show a version only when it is an older one; call `set_version` when the project moves on."
                    ),
                    None => "Current working version: not set — call `set_version` once you know it (a release tag, a date, anything).".to_string(),
                };
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
            }

            // Handoff notes ([`crate::config::HANDOFF_TAG`]): what the last session left
            // for this one, always on top. The worklist is fetched once and partitioned.
            let (handoff, worklist): (Vec<Node>, Vec<Node>) = self
                .store
                .list_open(&[])?
                .into_iter()
                .partition(|n| n.tags.iter().any(|t| t == crate::config::HANDOFF_TAG));
            // The current cycle: open worklist notes stamped with the
            // working version lead, right under the version line — what this
            // cycle set out to do and still owes. Handoff notes keep their own
            // section; everything else stays in the open section below.
            let (cycle, worklist): (Vec<Node>, Vec<Node>) = match (&working_version, bc.cycle.show)
            {
                (Some(v), true) => worklist
                    .into_iter()
                    .partition(|n| n.version.as_deref() == Some(v.as_str())),
                _ => (Vec::new(), worklist),
            };
            if let Some(v) = working_version.as_deref().filter(|_| !cycle.is_empty()) {
                let heading = format!(
                    "\n## Current cycle\nOpen work stamped with the working version: what this cycle set out to do and still owes. `search` with `during_version: \"{v}\"` reads everything captured in it."
                );
                if !push_line(&mut out, &heading) {
                    break 'assemble;
                }
                for n in cycle.iter().take(bc.cycle.cap) {
                    let line = node_line_cfg(
                        n,
                        ex(bc.cycle.excerpt),
                        Some(&cfg),
                        working_version.as_deref(),
                    );
                    if !push_line(&mut out, &line) {
                        break 'assemble;
                    }
                    seen.insert(n.id.clone());
                    included.push(n.id.clone());
                }
                if cycle.len() > bc.cycle.cap {
                    let line = format!(
                        "- …and {} more of this cycle — `list_open` has the full worklist.",
                        cycle.len() - bc.cycle.cap
                    );
                    if !push_line(&mut out, &line) {
                        break 'assemble;
                    }
                }
            }
            // Tasks: the working list beside the graph — what the
            // assistant was doing, titles only. Rules settle on the way in.
            for line in self.brief_task_lines()? {
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
            }
            if bc.handoff.show {
                if !handoff.is_empty()
                    && !push_line(
                        &mut out,
                        "\n## Handoff — left for this session, read first\nAct on each, then mark it resolved (`update_node` status resolved).",
                    )
                {
                    break 'assemble;
                }
                for n in handoff.iter().take(bc.handoff.cap) {
                    let line = node_line_cfg(
                        n,
                        ex(bc.handoff.excerpt),
                        Some(&cfg),
                        working_version.as_deref(),
                    );
                    if !push_line(&mut out, &line) {
                        break 'assemble;
                    }
                    seen.insert(n.id.clone());
                    included.push(n.id.clone());
                }
            }

            // The live tag vocabulary, up front: one cheap line the writing
            // assistant must see (a budget-cut tail section never surfaces on
            // a mature graph). A genuinely new tag is fine — created on write.
            let tags = if bc.tags.show {
                self.store.tag_stats(bc.tags.cap)?
            } else {
                Vec::new()
            };
            if !tags.is_empty() {
                let list = tags
                    .iter()
                    .map(|t| t.tag.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let line = format!("Recent tags (reuse before inventing new ones): {list}");
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
            }

            let conflicts = if bc.conflicts.show {
                self.store.active_conflict_edges()?
            } else {
                Vec::new()
            };
            if !conflicts.is_empty() && !push_line(&mut out, "\n## Unresolved conflicts") {
                break 'assemble;
            }
            for e in conflicts {
                let (Some(a), Some(b)) = (
                    self.store.get_node(&e.from_id)?,
                    self.store.get_node(&e.to_id)?,
                ) else {
                    continue;
                };
                let line = format!(
                    "- \"{}\" [{} {}] conflicts with \"{}\" [{} {}]",
                    a.title,
                    a.node_type.as_str(),
                    a.id,
                    b.title,
                    b.node_type.as_str(),
                    b.id,
                );
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
                for n in [a, b] {
                    if seen.insert(n.id.clone()) {
                        included.push(n.id);
                    }
                }
            }

            let mut suspects = if bc.suspects.show {
                self.store.suspects_pending()?
            } else {
                Vec::new()
            };
            // NLI-hinted contradictions first (the pairs most worth the
            // judge's attention), then strongest similarity; capped — the
            // brief is a digest, the full queue lives in list_suspects/pane.
            suspects.sort_by(|x, y| {
                let contra = |s: &SuspectView| s.nli_label.as_deref() == Some("contradiction");
                contra(y)
                    .cmp(&contra(x))
                    .then(y.similarity.total_cmp(&x.similarity))
            });
            let overflow = suspects.len().saturating_sub(bc.suspects.cap);
            suspects.truncate(bc.suspects.cap);
            if !suspects.is_empty() {
                let heading = "\n## Suspected conflicts — judge these\nThe local scan flagged \
                     unlinked look-alike pairs. For each: `resolve_suspect(id, verdict)` with \
                     `conflict` (they contradict), `replaces` (the newer supersedes — archives \
                     the older), or `dismiss` (unrelated/fine together). A hint of `inherited` \
                     means the newer note contradicts or replaces something the other one builds \
                     on, needs, or holds because of — follow its edges before judging. A hint of \
                     `bridged` means the two notes are about different subjects that the `via` \
                     note ties together — they clash only if it holds.";
                if !push_line(&mut out, heading) {
                    break 'assemble;
                }
                for s in suspects {
                    let hint = match (&s.nli_label, s.nli_score) {
                        (Some(label), Some(score)) => {
                            let side = match s.nli_direction.as_deref() {
                                Some(side) => format!(", negation likely on the {side} side"),
                                None => String::new(),
                            };
                            format!("; hint: {label} {:.0}%{side}", score * 100.0)
                        }
                        _ => String::new(),
                    };
                    let line = format!(
                        "- {}: \"{}\" [{} {}] vs \"{}\" [{} {}] ({:.0}% similar{hint})",
                        s.id,
                        s.a.title,
                        s.a.node_type.as_str(),
                        s.a.id,
                        s.b.title,
                        s.b.node_type.as_str(),
                        s.b.id,
                        s.similarity * 100.0,
                    );
                    let line = match &s.via {
                        Some(v) => format!("{line} via \"{}\" [{}]", v.title, v.id),
                        None => line,
                    };
                    if !push_line(&mut out, &line) {
                        break 'assemble;
                    }
                }
                if overflow > 0
                    && !push_line(
                        &mut out,
                        &format!("- …and {overflow} more — `list_suspects` has the full queue."),
                    )
                {
                    break 'assemble;
                }
            }

            // What changed lately, right after the judgment queue: recency is
            // the context the assistant continues from, so it must never fall
            // into the budget-cut tail. A node shown here is claimed — later
            // sections skip it rather than repeat it.
            let recent: Vec<Node> = if bc.recent.show {
                self.store
                    .recent_nodes(bc.recent.cap)?
                    .into_iter()
                    .filter(|n| !seen.contains(&n.id))
                    .collect()
            } else {
                Vec::new()
            };
            if !recent.is_empty() && !push_line(&mut out, "\n## Recently added") {
                break 'assemble;
            }
            for n in recent {
                let line = node_line_cfg(
                    &n,
                    ex(bc.recent.excerpt),
                    Some(&cfg),
                    working_version.as_deref(),
                );
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
                seen.insert(n.id.clone());
                included.push(n.id);
            }

            // Newest first (list_open's order), capped: the brief samples the
            // worklist, it doesn't mirror it — uncapped, a dogfood-sized
            // worklist ate a third of the budget and starved every later
            // section. The overflow line keeps the full count honest.
            let open: Vec<Node> = if bc.open.show {
                worklist
                    .into_iter()
                    .filter(|n| !seen.contains(&n.id))
                    .collect()
            } else {
                Vec::new()
            };
            // "## Open problems & intents" in the shipped set — the heading
            // names whatever types carry the worklist role here.
            let open_heading = format!(
                "\n## Open {}",
                cfg.worklist_types()
                    .iter()
                    .map(|t| format!("{}s", t.to_lowercase()))
                    .collect::<Vec<_>>()
                    .join(" & ")
            );
            if !open.is_empty() && !push_line(&mut out, &open_heading) {
                break 'assemble;
            }
            for n in open.iter().take(bc.open.cap) {
                let line = node_line_cfg(
                    n,
                    ex(bc.open.excerpt),
                    Some(&cfg),
                    working_version.as_deref(),
                );
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
                seen.insert(n.id.clone());
                included.push(n.id.clone());
            }
            if open.len() > bc.open.cap {
                let line = format!(
                    "- …and {} more — `list_open` has the full worklist.",
                    open.len() - bc.open.cap
                );
                if !push_line(&mut out, &line) {
                    break 'assemble;
                }
            }

            // Live-edge counts for `canon_order: connected`, computed once
            // on first use.
            let mut degree: Option<std::collections::HashMap<String, usize>> = None;
            // The per-type canon sections, in ontology order (the shipped set
            // shows Principles, then Decisions with a shorter excerpt —
            // their titles are already declarative — then Cautions).
            for t in cfg.ontology.types.iter().filter(|t| t.brief.show) {
                let heading = format!("\n## {}s", t.name);
                let node_type = NodeType::parse(&t.name)?;
                let (cap, excerpt) = (t.brief.cap, t.brief.excerpt);
                // Count the full active set, not a capped window, so nodes claimed by earlier
                // sections don't starve this one or get double-counted as "more".
                let total = self.store.count_by_type_active(&node_type)? as usize;
                let mut fetched = self.store.nodes_by_type_active(&node_type, total)?;
                if bc.canon_order == "connected" {
                    let degree = degree.get_or_insert_with(|| self.live_degrees());
                    sort_connected(&mut fetched, degree);
                }
                let elsewhere = fetched.iter().filter(|n| seen.contains(&n.id)).count();
                let nodes: Vec<Node> = fetched
                    .into_iter()
                    .filter(|n| !seen.contains(&n.id))
                    .take(cap)
                    .collect();
                if !nodes.is_empty() && !push_line(&mut out, &heading) {
                    break 'assemble;
                }
                let shown = nodes.len();
                for n in nodes {
                    let line =
                        node_line_cfg(&n, ex(excerpt), Some(&cfg), working_version.as_deref());
                    if !push_line(&mut out, &line) {
                        break 'assemble;
                    }
                    seen.insert(n.id.clone());
                    included.push(n.id);
                }
                // The cap hides real canon; say how much, so the assistant
                // knows the section is a sample, not the whole set.
                if total > shown + elsewhere {
                    let line = format!(
                        "- …{} more {}s — `search` reaches them.",
                        total - shown - elsewhere,
                        node_type.as_str()
                    );
                    if !push_line(&mut out, &line) {
                        break 'assemble;
                    }
                }
            }
        }

        // Cold start: an empty brief teaches
        // the assistant to offer seeding instead of reporting nothing.
        if included.is_empty() && self.store.all_nodes()?.is_empty() {
            out.push_str(COLD_START_BRIEF);
            // A cold graph is exactly where the writer has never seen the
            // ontology (issue #9: a model with no loaded skill guessed at
            // types until it grepped the schema) — teach this graph's actual
            // set in the same breath, unless the brief already led with it.
            if !bc.ontology.show {
                out.push('\n');
                out.push_str(&cfg.describe_ontology());
            }
        }

        self.store.touch(&included)?;
        // Activity journal: a served brief is the trace of a session starting
        // work (whoever asked — hook, MCP tool, pane lens, CLI).
        self.audit_activity(
            "brief_served",
            Some(format!(
                "{} chars, {} nodes included",
                out.len(),
                included.len()
            )),
        )?;
        Ok(out)
    }

    // ---- conflict scan: detection is local and automatic; judgment
    // stays with Claude in-session or the user in the pane. The daemon never
    // calls an LLM.
}

/// Appended to the brief when the graph is empty, so a cold start reads as an
/// actionable instruction to the assistant rather than an empty digest.
const COLD_START_BRIEF: &str = "\nThe graph is empty — this is a cold start.\n\n\
Offer the user a one-time seeding pass (ask first; this is the one capture \
that must not be silent): read the project's existing canon — README, \
design/plan docs, recent git history — and batch-capture the durable \
knowledge as provisional nodes: key Decisions with their reasons (`because` \
edges), stated Principles and conventions, known Cautions, and open Intents, \
attached to Anchors where several notes share a subject. Afterward, point the \
user at the pane to review what was captured. If the user declines, don't ask \
again — just capture knowledge as it emerges.\n\n\
Before seeding, review the settings with the user: `GET /guide` on the core \
(port in `.engram/daemon.json`) is the manual — does the shipped ontology fit, \
or would a preset, a custom set, or custom fields fit better? A customized \
graph wants `brief.ontology.show` on, so every session learns its vocabulary.\n";

/// Longest `replaces`-path from a timeline node down to an original (which is
/// generation 0). Memoized; a cycle (bad data) counts as 0 instead of hanging.
pub(super) fn generation<'a>(
    id: &'a str,
    adj: &std::collections::HashMap<&'a str, Vec<&'a str>>,
    memo: &mut std::collections::HashMap<&'a str, usize>,
) -> usize {
    if let Some(&g) = memo.get(id) {
        return g;
    }
    memo.insert(id, 0); // cycle guard while this node is being computed
    let g = adj.get(id).map_or(0, |olders| {
        olders
            .iter()
            .map(|o| generation(o, adj, memo) + 1)
            .max()
            .unwrap_or(0)
    });
    memo.insert(id, g);
    g
}

/// Longest excerpt per brief line, cut at a word boundary. 240 ran the budget
/// out mid-Cautions; 140 keeps the leading sentence and every section.
pub const EXCERPT_CHARS: usize = 140;

/// One brief line per node, one uniform shape everywhere:
/// `- Title [Type id status STALE] — excerpt`. Every record carries its id so
/// the assistant can act on it directly (`get_node`, `traverse`,
/// `update_node`) without a `search` round-trip.
pub fn node_line(n: &Node, excerpt_max: usize) -> String {
    node_line_cfg(n, excerpt_max, None, None)
}

/// [`node_line`] with the graph's config in hand: custom fields flagged
/// `show_in_brief` render as `{name: value; …}` before the excerpt.
/// A version stamp equal to `working` is left out: the brief's first
/// line already names the working version, so only older stamps say anything.
pub fn node_line_cfg(
    n: &Node,
    excerpt_max: usize,
    cfg: Option<&crate::config::GraphConfig>,
    working: Option<&str>,
) -> String {
    let mut line = format!("- {} [{} {}", n.title, n.node_type.as_str(), n.id);
    if let Some(version) = n.version.as_deref().filter(|v| Some(*v) != working) {
        line.push(' ');
        line.push_str(version);
    }
    if let Some(status) = n.status {
        line.push(' ');
        line.push_str(status.as_str());
    }
    if n.trust_override.is_some() {
        line.push_str(" PINNED");
    }
    if n.stale {
        line.push_str(" STALE");
    }
    line.push(']');
    if let Some(cfg) = cfg
        && let Some(fields) = &n.fields
    {
        let shown: Vec<String> = cfg
            .fields
            .iter()
            .filter(|f| f.show_in_brief)
            .filter_map(|f| {
                fields.get(&f.name).map(|v| {
                    let text = match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    format!("{}: {}", f.name, text)
                })
            })
            .collect();
        if !shown.is_empty() {
            line.push_str(&format!(" {{{}}}", shown.join("; ")));
        }
    }
    if let Some(body) = n
        .body
        .as_deref()
        .filter(|b| !b.is_empty() && excerpt_max > 0)
    {
        line.push_str(" — ");
        line.push_str(&excerpt_words(&body.replace('\n', " "), excerpt_max));
    }
    line
}

/// `canon_order: connected`: pinned first, then trust × (1 +
/// ln(1 + live edges)) — the canon other knowledge leans on leads — with the
/// endorsed order (the input's) breaking ties.
fn sort_connected(nodes: &mut [Node], degree: &std::collections::HashMap<String, usize>) {
    let weight = |n: &Node| {
        let d = degree.get(&n.id).copied().unwrap_or(0) as f64;
        n.trust * (1.0 + (1.0 + d).ln())
    };
    nodes.sort_by(|a, b| {
        b.trust_override
            .is_some()
            .cmp(&a.trust_override.is_some())
            .then(weight(b).total_cmp(&weight(a)))
    });
}

/// Whether a node's type carries the `anchor` role under this graph's
/// ontology (code-subject labels: similar by nature, not by contradiction).
pub(super) fn is_anchor(cfg: &crate::config::GraphConfig, n: &Node) -> bool {
    cfg.type_def(n.node_type.as_str())
        .is_some_and(|t| t.roles.anchor)
}

/// Cut text at the last word boundary within `max` chars, appending `…` when
/// anything was dropped.
pub(super) fn excerpt_words(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    let trimmed = match cut.rfind(char::is_whitespace) {
        Some(i) if i > max / 2 => &cut[..i],
        _ => cut.as_str(),
    };
    format!("{}…", trimmed.trim_end())
}
