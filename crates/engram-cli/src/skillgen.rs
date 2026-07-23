//! Skill generation from a graph's ontology (PLAN §7D teaching surface):
//! when a graph runs the shipped ontology the canonical variant text is the
//! best skill there is — install it verbatim. When the ontology is
//! customized, generate a skill that teaches THIS graph's types, verbs and
//! policy in the same voice, so the assistant never has to translate the
//! shipped examples by hand.

use std::path::Path;

use engram_core::GraphConfig;
use engram_core::config::{TypeDef, VerbDef};

use crate::setup::{claude_skill, is_symlink};

/// The skill text for `cfg`: `(content, generated)` — `generated: false`
/// means the shipped ontology is in force and the canonical variant text was
/// used verbatim.
pub fn generate(cfg: &GraphConfig, variant: &str) -> (String, bool) {
    if cfg.ontology == GraphConfig::default().ontology {
        return (claude_skill(variant).to_string(), false);
    }
    (generate_custom(cfg, variant), true)
}

/// Install the skill into `repo_root/.claude/skills/engram`, mirroring
/// `setup`'s rules — including the symlink guard: a symlinked skill dir
/// points into someone's source tree and must never be written through.
/// The guard is a *report*, not an error: a symlinked skill means the
/// project already sources its skill deliberately (this repo dogfoods that
/// way), so the response says so and names the target.
pub struct SkillInstaller;

impl engram_http::SkillAdmin for SkillInstaller {
    fn install(
        &self,
        repo_root: &Path,
        cfg: &GraphConfig,
        variant: &str,
    ) -> engram_core::Result<serde_json::Value> {
        let dir = repo_root.join(".claude/skills/engram");
        for link in [dir.clone(), dir.join("SKILL.md")] {
            if is_symlink(&link) {
                let target = std::fs::read_link(&link)
                    .map(|t| t.display().to_string())
                    .unwrap_or_else(|_| "its target".into());
                return Ok(serde_json::json!({
                    "installed": false,
                    "symlink": true,
                    "path": link.display().to_string(),
                    "target": target,
                    "note": format!(
                        ".claude/skills/engram is a symlink to {target} — this project sources \
                         its skill from there deliberately, so it was left untouched. Edit the \
                         linked file to change the skill."
                    ),
                }));
            }
        }
        let (content, generated) = generate(cfg, variant);
        std::fs::create_dir_all(&dir)
            .map_err(|e| engram_core::Error::Config(format!("couldn't create {dir:?}: {e}")))?;
        let path = dir.join("SKILL.md");
        std::fs::write(&path, content)
            .map_err(|e| engram_core::Error::Config(format!("couldn't write {path:?}: {e}")))?;
        Ok(serde_json::json!({
            "installed": true,
            "path": path.display().to_string(),
            "generated": generated,
            "variant": variant,
        }))
    }
}

// Keep in sync with frontend/src/constants/trust.ts (pct/humanDays): same
// thresholds and divisors, so the pane and generated skills speak alike.
fn pct(v: f64) -> String {
    format!("{}%", (v * 100.0).round() as i64)
}

fn days(d: i64) -> String {
    match d {
        0..=13 => format!("{d} days"),
        14..=59 => format!("~{} weeks", (d as f64 / 7.0).round() as i64),
        60..=699 => format!("~{} months", (d as f64 / 30.4).round() as i64),
        _ => format!("~{} years", (d as f64 / 365.0).round() as i64),
    }
}

fn type_line(t: &TypeDef) -> String {
    let mut line = format!("- **{}** — {}.", t.name, t.thought);
    if t.roles.worklist {
        line.push_str(" Lives on the open worklist: born with `status: open`, closed when settled — it never decays while open.");
    }
    if t.roles.anchor {
        line.push_str(" A code-subject label: carries `code_refs` as its identity; attach related notes to it rather than writing prose about files.");
    }
    line
}

fn verb_line(v: &VerbDef, worklist_names: &str) -> String {
    let mut line = format!("- `{}` — {}.", v.name, v.reads_as);
    if v.roles.reason {
        line.push_str(" The reason edge: reasoning nodes without one are what the checkup flags.");
    }
    if v.roles.answer {
        line.push_str(&format!(
            " Closes worklist items — after linking, set the answered {worklist_names}'s status to resolved."
        ));
    }
    if v.roles.supersession {
        line.push_str(" Supersession: creating it archives the older endpoint but keeps it as history. Put the *why of the change* in the edge note — `timeline` shows it later.");
    }
    if v.roles.contradiction {
        line.push_str(" The contradiction edge — **high value, always create it** when two nodes genuinely contradict; a judged one demotes the older claim's trust, and it is the one capture worth mentioning aloud in chat.");
    }
    if v.roles.dependency {
        line.push_str(" A live dependency / blocker.");
    }
    line
}

fn generate_custom(cfg: &GraphConfig, variant: &str) -> String {
    let o = &cfg.ontology;
    let p = &cfg.policy;
    let type_names: Vec<&str> = o.types.iter().map(|t| t.name.as_str()).collect();
    let worklist: Vec<&str> = o
        .types
        .iter()
        .filter(|t| t.roles.worklist)
        .map(|t| t.name.as_str())
        .collect();
    let worklist_names = if worklist.is_empty() {
        "item".to_string()
    } else {
        worklist.join("/")
    };
    let supersession = cfg.supersession_verb();
    let contradiction = cfg.contradiction_verb();

    let mut by_durability: Vec<String> = Vec::new();
    for (label, want) in [
        ("stable", engram_core::Durability::Stable),
        ("episodic", engram_core::Durability::Episodic),
        ("volatile", engram_core::Durability::Volatile),
    ] {
        let names: Vec<&str> = o
            .types
            .iter()
            .filter(|t| t.durability == want)
            .map(|t| t.name.as_str())
            .collect();
        if !names.is_empty() {
            by_durability.push(format!("{} → `{label}`", names.join("/")));
        }
    }

    let intensity = match variant {
        "aggressive" => {
            "Capture liberally: every real decision, realization, and gotcha belongs in the graph. Err on the side of writing — decay prunes scratch that never gets re-confirmed, while a missing node costs a repeated mistake."
        }
        "normal" => {
            "Capture the durable middle ground: real decisions, non-obvious realizations, and anything that bit you. Skip play-by-play."
        }
        _ => {
            "Capture sparingly but reliably: settled decisions, standing rules, and gotchas. When in doubt on small things, let it go — but never drop a genuine decision."
        }
    };

    let types_block = o.types.iter().map(type_line).collect::<Vec<_>>().join("\n");
    let verbs_block = o
        .verbs
        .iter()
        .map(|v| verb_line(v, &worklist_names))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r##"---
name: engram
description: Read and write this project's durable reasoning memory ({types_csv}) through the Engram MCP tools. Recall relevant memory before non-trivial work; capture knowledge silently at natural stopping points; keep the graph honest. Generated from this graph's customized ontology (preset "{preset}").
---

# Engram — project memory (generated from this graph's ontology)

Engram is a local, user-owned graph of *why things are the way they are* in this project. This graph runs a **customized ontology** — the types and verbs below are its actual vocabulary, and this file was generated from it. If a write is refused with "unknown node type" or the names here surprise you, the ontology changed after generation: call `describe_ontology` for the live definition and ask the user to regenerate this skill from the pane's Graph settings.

{intensity}

## Recall — brief first, then search

- **At the start of a session**, call `brief` once — a compact digest of the canon. If the session already opens with an injected "# Engram brief", that IS the brief; don't call it again.
- Before any **non-trivial decision**, `search` with a natural-language description of what you're about to do. Hits carry their 1-hop neighbors, `{contradiction}`/`{supersession}` first — read those especially.
- `get_node` / `traverse` pull the reasoning around a hit; `timeline` walks a node's `{supersession}` chain oldest-first; `list_open` shows the live worklist (open {worklist_csv}).

## Capture — this graph's node types

{types_block}

**Never save:** secrets, credentials, PII — ever. No volatile implementation detail (line numbers, transient state) unless asked. No mirrors of what code or git history already record.

## How to write

1. **Avoid duplicates proportionally.** `add_note` self-checks similarity and returns `{{ matched, created: false }}` on a near-dupe — then `update_node` the match. Search first when the topic is plausibly already covered.
2. **Pick the type from the list above.** This graph defines exactly {n_types}: {types_csv}. Nothing else will be accepted.
3. **Title**: short and declarative. **Body**: the reasoning in 1–3 sentences — the *why*, not a transcript.
4. **Link it.** Edges must read as an English sentence, using this graph's verbs:
{verbs_block}
   - If you can't complete the sentence with one of these verbs, don't link. An honestly unlinked node beats a forced edge.
5. **The write response is a verdict, not a receipt — act on it in the same turn:**
   - `{{ matched, created: false }}` — merge into the match with `update_node`; never re-add.
   - `warnings` — your note landed near contradicted or superseded knowledge. Read the flagged node; align, or record the disagreement deliberately (`{contradiction}` / `{supersession}`).
   - `suspects` — look-alike pairs queued for judgment. Judge each NOW with `resolve_suspect`: they contradict → `conflict` (creates a `{contradiction}` edge — and say so in chat, the one exception to silent capture); your note supersedes → `replaces` (creates a `{supersession}` edge and archives the older); fine together → `dismiss`.
   - `canon` — NLI verdicts from nearby existing knowledge: `supports` (canon backs your text — link it) or `contradicts` (canon disputes it — read the flagged node; a real disagreement becomes a `{contradiction}` edge and gets said in chat).
6. **Repair mislinks** with `unlink` / `update_edge` — a wrong edge is yours to fix.

## Durability — let it default

Types default their durability here: {durability_lines}. Don't override durability to `volatile` on your own.

## Trust & staleness

Nodes carry computed `trust` (0..1) under this graph's tuned policy: a fresh assistant note starts at {t_created}; a deliberate update or "confirm still true" lifts it to {t_confirmed}; user approval sets {t_approved}. Unapproved episodic notes fade over {d_episodic}, volatile over {d_volatile}; stable knowledge never fades with time — only a judged `{contradiction}` demotes it. Below {t_stale} a node is `stale`: verify before relying, and if it is still true, say so with `update_node` — that restores trust. Retrieval never refreshes trust; being findable proves nothing. PINNED nodes are user-locked: a `replaces` verdict that would archive one is refused — surface it to the user.

## Maintenance — keep the graph honest

- Judge suspected conflicts early (`list_suspects` / the brief's queue) — the scan only nominates; you are the judge.
- Close the loop: when work settles an open {worklist_names}, link the settling node with the answer verb and set the item resolved.
- Repair drifted `code_refs` (`list_drift`) when files move.
- If the brief opens with "Current working version", new notes are stamped with it automatically; call `set_version` when the project moves to a new version.
- A note the NEXT session must see first: an open worklist note tagged `handoff` — the brief tops with it; resolve it once acted on.

## Multi-project memory

Most tools take an optional `project`: omit = this project, a name = that project's graph (capture knowledge about a sibling project into ITS graph), `home` = the user's machine-level canon ("remember this globally"), `all` = cross-project reads on search/check_claim. Never create edges across graphs.
"##,
        preset = o.preset,
        n_types = o.types.len(),
        types_csv = type_names.join(", "),
        worklist_csv = if worklist.is_empty() {
            "items".to_string()
        } else {
            worklist.join("s, ") + "s"
        },
        t_created = pct(p.trust_created),
        t_confirmed = pct(p.trust_confirmed),
        t_approved = pct(p.trust_approved),
        t_stale = pct(p.stale_trust),
        d_episodic = days(p.episodic_window_days),
        d_volatile = days(p.volatile_window_days),
        durability_lines = by_durability.join("; "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom() -> GraphConfig {
        let mut cfg = GraphConfig::default();
        cfg.ontology.preset = "custom".into();
        cfg.ontology.types[0].name = "Rule".into();
        cfg.ontology.types[0].thought = "a law of this project".into();
        cfg.ontology.verbs = cfg
            .ontology
            .verbs
            .into_iter()
            .map(|mut v| {
                if v.roles.supersession {
                    v.name = "supersedes".into();
                    v.reads_as = "Rule supersedes Rule".into();
                }
                v
            })
            .collect();
        cfg
    }

    #[test]
    fn default_ontology_installs_the_canonical_variant_verbatim() {
        let cfg = GraphConfig::default();
        for variant in ["relaxed", "normal", "aggressive"] {
            let (text, generated) = generate(&cfg, variant);
            assert!(!generated);
            assert_eq!(text, claude_skill(variant));
        }
    }

    #[test]
    fn custom_ontology_generates_a_skill_in_its_own_vocabulary() {
        let (text, generated) = generate(&custom(), "aggressive");
        assert!(generated);
        assert!(text.contains("- **Rule** — a law of this project."));
        assert!(text.contains("`supersedes`"));
        assert!(!text.contains("**Principle**"), "renamed types never leak");
        assert!(text.contains("describe_ontology"));
        assert!(text.contains("exactly 8: Rule, Decision"));
        // Policy numbers ride in as plain words.
        assert!(text.contains("starts at 50%"));
        assert!(text.contains("~6 months"));
    }

    #[test]
    fn installer_reports_symlinked_skill_dirs_untouched() {
        use engram_http::SkillAdmin;
        let tmp = std::env::temp_dir().join(format!("engram-skillgen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("real")).unwrap();
        std::fs::create_dir_all(tmp.join(".claude/skills")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(tmp.join("real"), tmp.join(".claude/skills/engram")).unwrap();
        #[cfg(unix)]
        {
            // Not an error: the symlink is deliberate sourcing — report it,
            // name the target, and leave the tree untouched.
            let out = SkillInstaller
                .install(&tmp, &GraphConfig::default(), "relaxed")
                .unwrap();
            assert_eq!(out["installed"], false);
            assert_eq!(out["symlink"], true);
            assert!(out["target"].as_str().unwrap().ends_with("real"));
            assert!(
                !tmp.join("real/SKILL.md").exists(),
                "nothing written through the link"
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn installer_writes_the_skill_file() {
        use engram_http::SkillAdmin;
        let tmp = std::env::temp_dir().join(format!("engram-skillgen-w-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out = SkillInstaller.install(&tmp, &custom(), "normal").unwrap();
        assert_eq!(out["installed"], true);
        assert_eq!(out["generated"], true);
        let path = out["path"].as_str().unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("- **Rule** — a law of this project."));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
