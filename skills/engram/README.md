# Engram capture skill — three variants

The skill that teaches Claude when to read from and write to the Engram graph ships in three self-contained variants. **Install exactly one** (they all register as the `engram` skill):

| Variant | Capture policy | Install when |
|---|---|---|
| [`relaxed/`](./relaxed/SKILL.md) | Only durable, high-value knowledge: principles, major decisions, genuinely hard problem/resolutions. Fewer, better nodes. | **Recommended default.** |
| [`normal/`](./normal/SKILL.md) | + cautions, selective insights, finer-grained decisions, intents worth carrying. | You want a fuller graph without the firehose. |
| [`aggressive/`](./aggressive/SKILL.md) | Everything: every decision, insight, proactive caution, TODO. Engram becomes the spine of the project's decision history; trust decay prunes episodic scratch that never gets re-confirmed. | Dogfooding, heavy multi-session projects, or growing a knowledge base fast. |

## Install

Copy the chosen variant into your Claude Code skills directory as `engram`:

```sh
# per-project
mkdir -p .claude/skills && cp -R skills/engram/relaxed .claude/skills/engram

# or globally
mkdir -p ~/.claude/skills && cp -R skills/engram/relaxed ~/.claude/skills/engram
```

Switch modes by replacing the installed copy with another variant. `engram-alpha setup --cli claude --skill <variant>` does this for you (relaxed by default).

All variants share the same recall behavior, ontology rules, secret/PII prohibition, and silent-batched write etiquette — they differ only in how much is worth a node.

## The digest skill

[`digest/`](./digest/SKILL.md) is a separate, variant-less skill (`engram-digest`) for one job: explicitly ingesting an existing project into a fresh or thin graph (PLAN §7B). It is invoked on demand ("digest this project", `/engram:digest` with the plugin) — deliberately not folded into the capture skill, so its heavyweight ontology examples don't sit in every session's context. Install it alongside whichever capture variant you chose (`engram-alpha setup` does both):

```sh
mkdir -p .claude/skills && cp -R skills/engram/digest .claude/skills/engram-digest
```
