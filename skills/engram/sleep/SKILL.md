---
name: engram-sleep
description: Consolidate an Engram graph ("sleep") — settle standing conflicts, work the subjects (named piles of close notes) into one state-of note each, merge restatements, link what supersedes what, tie islands (notes nothing reaches) back in, and mark the rest reviewed. On demand only - use when the user asks to sleep, consolidate, tidy or deduplicate memory, invokes /engram-sleep, or complains about too many, duplicate or noisy memories. Not for routine capture — the engram skill covers that.
---

# Engram sleep — consolidate after the fact

A graph that is used grows piles: the same subject restated across versions, an investigation captured as twenty findings, a decision and three paraphrases of it, a release note per version. Every pile costs twice. Search delivers several near-identical notes where one would do, and the current answer competes with its own history for the agent's attention. And nobody can review thousands of notes by hand. Sleep is the cure, applied after the fact: read one subject, write down where it stands now, keep one clear note per claim, record what replaced what, and leave the rest alone. Two smaller queues ride along: conflicts nobody settled, and islands — notes no edge and no tag reaches, which only a lucky text search ever finds.

The daemon nominates; you judge. Engram measures closeness from the stored vectors (no model call):
- The **band** is the cosine this graph calls close: its own top 1% of note pairs. It means "closer than almost everything else here", not a fixed number.
- A **subject** is a group of notes joined by chains of close pairs: at least 2% of the graph's notes (never fewer than 5), at most four times that. When subjects touch and chain into a larger blob, Engram re-cuts it at a stricter cosine until the pieces fit; `cohesion` is the cosine a subject holds together at.
- Each subject has a **name**: two or three terms most of its notes share and the rest of the graph rarely uses (picked without a model; check it against the titles).
- The queue is ranked by **worth**: size × (1 + ln of the versions it spans) × how **settled** it is (members from past versions, older than two weeks). History spread over many releases comes first; a pile of this cycle's notes is live work and ranks low.

## When to run

- The user asks: "sleep", "consolidate memory", "tidy the graph", "dedupe", `/engram-sleep [subject]` (`/engram:sleep` in the plugin).
- **Suggest it** in one line, run it on a yes: the user complains about reviewing or too many, duplicate or noisy memories; or `search` keeps returning restatements of one subject and you can't tell which is current (offer `/engram-sleep <that subject>`).
- Never on your own, and never in the middle of other work.

## The three queues, in order

1. **Conflicts** — `list_nodes {view: "conflicts"}`: `standing` contradiction edges (with which side is `newer`) and pending `suspects`. Small and decisive, so first.
2. **Subjects** — `list_nodes {view: "review"}`, one per page, most worth first. The bulk of a sleep.
3. **Islands** — `list_nodes {view: "islands"}`: notes no edge and no tag reaches, each with its three nearest notes. Last, because a subject pass ties many of them in on the way.

A full sleep runs all three; `/engram-sleep <subject>` runs only that subject. Work conflicts and islands inline (they are short); delegate subjects.

## Delegate to a fresh agent

A subject is a lot of reading (a page is one subject, ~20 KB). Keep it out of the main conversation: in Claude Code, start a general-purpose subagent with the brief below, one subagent per subject. In a harness without subagents, run the same procedure inline, one subject per run.

```
You are running an Engram sleep on project <project>. Goal: fewer, clearer notes without
losing any reason, number or history. Scope: <"the top subject of the review queue" |
"subject <tag / anchor id / note id>">.
Read it with list_nodes {view: "review"} (one subject per page; add tag: "<t>" for a tagged
subject, or start from list_nodes {around: "<id>"}). Follow the engram-sleep procedure:
decide whether it is settled history (worth a state-of note) or live work, write the
state-of note if it is, link it builds-on what stays and replaces what it fully covers,
merge restatements with merge_nodes, link replaces / conflicts-with / answers between
members, judge listed suspects with resolve_suspect (a state-of note raises some against
its own members: its links settle them, dismiss what still agrees), and mark every note left standing
with update_nodes [{id, reviewed: true}, …]. Never merge pinned notes, Problems into their
Resolutions, or notes that disagree; never drop a measured number or a caution. Report:
the state-of note (id + title), merges (survivor + victims), links added, notes kept,
anything the user must decide.
```

## Conflicts

For each **standing** conflict, read both notes (`get_node`), then:
- one side is now canon and the other is history → `link {from: canon, to: other, type: "replaces"}`, then `update_edge` the conflict to `resolved`;
- both still hold in different scopes (versions, platforms) → say so in the newer note's body, resolve the edge;
- a real open disagreement → leave it and list it in the report: the user decides.

For each **suspect**: `resolve_suspect` with `conflict`, `replaces` or `dismiss`, exactly as in routine capture. A suspect between members of one subject can wait for that subject's pass.

## Islands

For each island, its `nearest` list says where it belongs:
- a nearest note it builds on, answers, or is about → `link` it (one sentence-shaped verb);
- a restatement of a nearest note → merge it into that note (`merge_nodes`);
- no sensible link but a clear subject → add the tag that subject's notes carry (reuse the brief's tags);
- obsolete with nothing replacing it → a Tombstone "Removed: <title>" with the why, linked `replaces`;
- genuinely standalone (a lone principle, a one-off fact) → mark it `reviewed: true` and move on.

## Reading a subject

Each subject carries:
- `name`: two or three terms most members share and the rest of the graph rarely uses (no model picked them; check them against the titles);
- `size`, `cohesion`, `worth` (`score`, `versions`, `settled`, `span_days`), `versions` (oldest → newest), and `shared` anchors and tags (what it is about);
- `members`: the most central notes in full: type, title, snippet, version, trust, edges, crowding, similarity to the centre;
- `more`: the rest as id + type + title + version lines; `get_node` any of them before you change it;
- `pairs`: the closest member pairs, closest first, with the verb already joining them. Restatements hide here;
- `suspects`: pending conflict suspects between members.

Read the titles of every member first, in `members` and `more`, so you know the whole story before you touch anything.

## The procedure, per subject

**1. Settled or live?** `settled` near 1 with many `versions` is history: the subject has moved on, and its notes record how. Write a state-of note (step 2). `settled` low means this cycle's work is still moving: skip the state-of note, only merge true restatements (step 3), and mark the rest.

**2. The state-of note.** One `add_note`:
- type: Insight. Use Decision only when the subject's present state is one standing decision.
- title: `<Subject> as of <newest version>: <where it stands, one sentence>`
- body: the current answer first; then the standing rules and decisions, with their ids; then the numbers that still matter; then what was tried and dropped, one line each, with ids; last, a "History:" line naming the versions it spans.

Then link it:
- `builds-on` every member that stays: notes holding their own measurement, caution, or decision.
- `replaces` only members it **fully** covers, such as a release-progress note, a status update the summary now carries, or a restatement. Those are archived behind the edge and still readable. When in doubt, use `builds-on`: history is worth its space.

Expect suspects as soon as the note lands: the conflict check often reads a summary as a rival claim to the notes it summarises, sometimes as a "contradiction" with a high score. Your `builds-on` / `replaces` links settle the suspects between linked notes. Check any suspect that is still pending, and dismiss it when the summary and the member agree.

**3. The pairs.** Decide per pair, not per subject:

| What you see | Do |
|---|---|
| Two or more notes state **the same claim** (paraphrase, version restatement) | `merge_nodes`: survivor = the most current and most connected; write the merged title/body yourself (see below) |
| A newer note **revises** an older one (the claim changed) | `link {from: newer, to: older, type: "replaces"}`: the older one is archived, its history kept |
| Two notes **contradict** and neither is clearly newer canon | `link … "conflicts-with"` and tell the user; they decide |
| A Resolution/Decision **answers** a Problem/Intent | `link {from: answer, to: problem, type: "answers"}`; never merge them |
| Same subject, **different claims** | keep both; link `builds-on` if one extends the other |
| A pending suspect between members | `resolve_suspect` (conflict / replaces / dismiss) |
| A note whose only content is now wrong and nothing replaces it | add a Tombstone "Removed: <title>" with the why, link it `replaces` the victim |

When unsure, open a member with `get_node` (full body, `parents`/`children`) or `list_nodes {around: id}` (nearest notes and subject-mates, no search cut-off), and keep walking until the picture is clear.

**Writing a merged note.** Title: the claim as it stands now, one sentence. Body: the current answer first, then the reasons from every part (each `because` survives), then a short "History:" line naming the versions it consolidates. Never drop a number, a measured result or a caution. `merge_nodes` keeps the victims archived behind `replaces` edges and moves their edges to the survivor, so links are not lost, but text you don't carry over is no longer in the live note.

**4. Mark it.** One `update_nodes` call marking every note left standing `reviewed: true`: members, merge survivors and the state-of note. The subject then leaves the queue while more than half its notes stay marked (a changed note loses its mark). A mark does not touch trust, and it is not "verified still true"; for that, update the note. Leave a subject unmarked if you cannot decide; it stays in the queue for the user.

Once a subject is done, it no longer appears at the top: `list_nodes {view: "review"}` with offset 0 gives the next one. Islands work the same way: a linked or tagged note stops being an island, and a reviewed one is hidden until it changes.

## Examples

**Settled history → state-of note.** A subject of 22 on `tag:tepindb`, versions 0.6.0 → 0.8.13, settled 1.0: the store move, captured as a plan, three blockers, a cutover, a registry rule, and five progress notes.
→ `add_note` Insight "The store move as of 0.8.13: every graph runs on TepinDB behind one machine core; redb's single-process lock is why the core owns all stores", with a body listing the standing decisions by id, the lock caution, and "History: 0.6.0–0.8.13".
→ `replaces` the five progress notes ("v0.6.2 scope: …", "cutover blocked: …" now resolved). `builds-on` the decisions and the caution, which keep their own reasons.
→ Mark everything left standing reviewed.

**Restatements inside a subject → merge.** In the pairs: "The bridge reconnects after a core restart" (Insight 0.10.4) at 0.94 to "Bridge waits 90 s for a restarted core" (Decision 0.10.4).
→ `merge_nodes {survivor: <Decision>, victims: [<Insight>], title: "Bridges wait 90 s for a lost or restarted core and never spawn one", body: …}`.

**An investigation → keep the findings, write the answer.** Twenty findings walk through "why oblique recall drops" and end in one measured answer.
→ Do not merge findings that carry different measurements. Write or find the one Insight that states the answer, link each finding `builds-on` it, and merge only the findings that report the same run twice.

**Revision → replaces.** "Default delivery floor is 0.22" (0.8.2) and "Delivery floor is calibrated per graph from phantom probes" (0.8.4).
→ The claim changed. Link `{from: 0.8.4 note, to: 0.8.2 note, type: "replaces"}`.

**Live work → leave it.** A subject of 14 notes from this cycle, settled 0.1.
→ No state-of note: the story isn't over. Merge only exact restatements, mark the rest, and report the subject as active.

## A subject sweep

`/engram-sleep bridge` or "sleep on the brief code":
1. Find the subject: a tag → `list_nodes {view: "review", tag: "<t>"}`; an anchor or a note → `list_nodes {around: "<id>"}` (`subject` lists everything sharing its anchors, tags and refs); free text → `search` first, then `around` its best hit.
2. Work it as above. For `around` results, the `close: true` neighbours are the candidates.

## Report

End with a short, audible summary. Sleep is the one Engram activity that is not silent:
- the state-of note: id and title, and how many notes it replaced or builds on;
- merges: survivor id, new title, how many notes it absorbed;
- conflicts settled and left for the user, suspects judged;
- islands linked, tagged, merged, buried or kept;
- links added (replaces / answers / conflicts-with);
- subjects kept and marked; subjects left for the user, and why;
- **undo:** every write of this sleep can be reverted at once in the pane. Open the Audit panel, open any row from this sleep, and choose "Undo session".
