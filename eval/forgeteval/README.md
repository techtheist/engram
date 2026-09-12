# ForgetEval — engram adapter

ForgetEval (arXiv:2606.15903, code at https://github.com/deeplethe/lethe under
`bench/forgeteval/`) is an MIT-licensed, judge-free benchmark for how well an
AI memory system *forgets*: it inscribes short facts into a system, applies a
mutation (a newer fact supersedes an older one, a fact is soft-evicted,
selectively forgotten, or hard-purged), then checks by **exact substring
match** whether the mutated fact is gone from the system's own top-k recall
and whether unrelated facts survived. No LLM judge is involved anywhere in
scoring. Two suites ship in the repo: **template** (5 families —
supersession / decay / amnesia / purge / drift — generated from templates;
1000 cases at `--scale 200`) and **adversarial** (385 hand-authored +
LLM-generated-and-oracle-validated cases across 10 attack categories:
substring traps, prefix collisions, paraphrase supersession, negation traps,
temporal qualifiers, shared attributes, compound facts, identifier
obfuscation, cross-lingual identifiers, recursive supersession).

This directory adapts **engram** (this repo's graph memory daemon) to
ForgetEval's `Adapter` protocol purely over its local HTTP API — no MCP, no
Rust code touched — and runs both suites against it. Two further adapters
extend the harness: **`engram-mcp`** (role-aware, same policy override as
`engram-notomb`, but `recall_texts` goes over the real MCP `search` TOOL —
the transport an actual agent client speaks — and records that tool's
confidence verdict per query) and **`grep`** (a daemon-free, embedding-free
keyword-overlap baseline). These two, plus a control-probe / hedge /
absence-signal measurement layered on top without touching ForgetEval's own
scoring, exist to answer a question the benchmark itself doesn't ask: *how
often does a system quietly make something up about a subject it was never
told about, and does engram's own "I'm not sure" signal actually show up
where it should?* See "Abstention: what ForgetEval does not measure" below.

## How to run

```bash
# one-time setup (already done in this checkout)
git clone --depth 1 https://github.com/deeplethe/lethe eval/data/lethe
python3 -m venv eval/data/venv
eval/data/venv/bin/pip install requests fastembed sqlite-vec
eval/data/venv/bin/pip install -e eval/data/lethe   # pylethe, for the reference adapter

# engram must already be running (a machine core on 127.0.0.1:8787 — see
# the project's own `engram-alpha serve`); this adapter never starts or
# stops it, and only ever touches its own throwaway project directories.

cd eval/forgeteval

# smoke (~20 cases/suite, sanity + timing check)
../data/venv/bin/python3 run_engram.py --adapter engram        --suite template    --scale 4  --project-dir /tmp/forgeteval-store
../data/venv/bin/python3 run_engram.py --adapter engram-notomb --suite adversarial --limit 20 --project-dir /tmp/forgeteval-store-notomb
../data/venv/bin/python3 run_engram.py --adapter engram-mcp    --suite template    --limit 20 --project-dir /tmp/forgeteval-store-mcp
../data/venv/bin/python3 run_engram.py --adapter grep          --suite adversarial --limit 20

# full receipts (as run for this record)
../data/venv/bin/python3 run_engram.py --adapter engram         --suite template    --scale 200 --project-dir <dir1> --out ../results/2026-09-12-forgeteval-template-engram.json
../data/venv/bin/python3 run_engram.py --adapter engram-notomb  --suite template    --scale 200 --project-dir <dir2> --out ../results/2026-09-12-forgeteval-template-engram-notomb.json
../data/venv/bin/python3 run_engram.py --adapter lethe          --suite template    --scale 200 --out ../results/2026-09-12-forgeteval-template-lethe.json
../data/venv/bin/python3 run_engram.py --adapter engram         --suite adversarial --project-dir <dir1> --out ../results/2026-09-12-forgeteval-adversarial-engram.json
../data/venv/bin/python3 run_engram.py --adapter engram-notomb  --suite adversarial --project-dir <dir2> --out ../results/2026-09-12-forgeteval-adversarial-engram-notomb.json
../data/venv/bin/python3 run_engram.py --adapter lethe          --suite adversarial --out ../results/2026-09-12-forgeteval-adversarial-lethe.json

python3 merge_receipts.py template    ../results/2026-09-12-forgeteval-template.json    ../results/2026-09-12-forgeteval-template-*.json
python3 merge_receipts.py adversarial ../results/2026-09-12-forgeteval-adversarial.json ../results/2026-09-12-forgeteval-adversarial-*.json
```

`--project-dir` must be a directory outside any registered engram project
(`engram`, `eval`, `home` are the developer's real graphs and are never
touched by this adapter). Two separate directories are used for `engram`
and `engram-notomb` so the two variants never share graph state.

## Files

- `engram_adapter.py` — the `EngramAdapter` class (see its module docstring
  for the full REST mapping, the query-resolution policy, and the config
  deviation) and `EngramMCPAdapter`, which reuses everything about
  `EngramAdapter(include_tombstones=False)` except `recall_texts`, which it
  routes over a real MCP `search` tool call (see that class's docstring for
  the transport details and a genuine gotcha it had to route around: an MCP
  session's engine handle is captured once at session creation and never
  rebinds, which silently served frozen pre-reset data until `reset()` was
  taught to force a fresh handshake every case).
- `grep_adapter.py` — the `GrepAdapter` class: a deterministic,
  embedding-free content-word-overlap baseline that mirrors the same
  supersede/release/purge target-resolution rules `EngramAdapter` mirrors
  from `LetheAdapter`.
- `run_engram.py` — wraps the upstream `bench/forgeteval/{generate,
  adversarial}.py` generators and `tests.py`'s `TestCase.run` /
  `GeneratedCase.run` scoring loop; nothing inside the `lethe` clone is
  patched. Also owns the control-probe generation, hedge computation, and
  absence-signal computation described below.
- `merge_receipts.py` — combines the per-adapter JSON receipts for one
  suite into the single `eval/results/<date>-forgeteval-<suite>.json` file,
  carrying the `controls`/`hedge`/`absence_signal` blocks through and
  printing the pass-rate-next-to-fp/hedge summary table.

## Semantics mapping

| ForgetEval primitive | engram operation |
|---|---|
| `reset()` | `rm <project>/.engram/graph.tepin`; the daemon's identity-checked engine cache (dev+inode) evicts the deleted store and reopens a fresh one lazily on the next request. Config (see below) lives *inside* the store file, so it reverts to defaults on every reset — the adapter re-applies its policy override immediately after. |
| `inscribe(text)` | `POST /nodes` `{type: "Insight", title: <first sentence of text, ≤120 chars>, body: text, durability: "episodic", source: "user"}`. |
| `recall_texts(query, k)` | `GET /search?q=<query>&limit=<k>`, hits already in score order. `engram` (the **role-blind** reader) returns every hit's text, Tombstone markers included; `engram-notomb` (the **role-aware** reader) asks the server for k non-tombstone hits (`types=` every type but the tombstone role), so markers never occupy top-k slots. Since 0.9.4 every hit also carries `tombstone: true` when it plays the role, which is the ontology-proof way to do the same filter client-side. |
| `supersede(old_query, new_text)` | Resolve `old_query` via `GET /search?limit=1` (types excluding Tombstone) — top-1, mirroring `LetheAdapter.supersede`'s `recall(k=1)`. `POST /nodes` the new fact, then `POST /edges {type: "replaces", from_id: new, to_id: old}`, which archives the old node (it drops out of search on its own — no extra step needed). No hit → just inscribe the new fact (same fallback as Lethe). |
| `release(query)` | Resolve a 20-deep search (types excluding Tombstone), then the **adaptive-gap threshold** over the `score` column (ported verbatim from `LetheAdapter._gap_threshold`, `min_gap=0.05`) selects the release set. Each match: `DELETE /nodes/{id}?tombstone=true&keep_text=false&reason=released`. |
| `purge(query)` | Same 20-deep search, then keep only hits whose **original inscribed text** (not the truncated search snippet) is NFKC/lower/whitespace-equivalent to the top hit's text (`LetheAdapter._norm_lexical`, ported verbatim). Each match: plain `DELETE /nodes/{id}` (hard delete, no tombstone). |

## Query-resolution policy — and why it mirrors Lethe specifically

`adapter.py`'s three reference implementations do not agree with each other
on how "release" or "purge" should resolve a natural-language query to a set
of target memories:

| op | Lethe | Mem0 | LangGraph |
|---|---|---|---|
| supersede | top-1 (`recall(k=1)`) | top-1 (`search(top_k=1)`) | top-1 (`search(limit=1)`) |
| release | hybrid recall(k=20) → **adaptive-gap threshold** on similarity | search(top_k=20) → adaptive-gap threshold on score (reuses `LetheAdapter._gap_threshold`) | search(limit=20) → adaptive-gap threshold on score (reuses the same function) |
| purge | lexical recall(k=20) → **NFKC/lower/whitespace dedup** against the top hit's text | search(top_k=20) → **adaptive-gap threshold** (same as release — no lexical step) | search(limit=20) → **raw string equality** against the top hit's text (no normalization) |

Supersede is unanimous. Release is unanimous (all three reuse
`_gap_threshold`). **Purge is the one place all three disagree**: Lethe
groups by normalized-identifier equality, Mem0 just reuses the release
policy, LangGraph does raw equality. Since Lethe is this benchmark's own
flagship/default adapter — the one `run.py` falls back to and the one the
paper's headline numbers are measured against — engram mirrors **Lethe's**
policy across all three operations rather than picking a majority vote or
inventing a fourth rule. This is a judgment call, stated here so it can be
second-guessed: it is the single most defensible "pick one" when the
references disagree, not a hidden thumb on the scale (all three ported
functions are literal, unmodified copies of `LetheAdapter`'s own code).

## Config deviation: the calibrated delivery floor is disabled

**This is the most consequential adapter decision in this exercise and is
reported prominently rather than buried.**

engram's `/search` endpoint applies a calibrated "delivery floor"
(`policy.delivery_floor`, default 0.22, plus `knee_cliff`, `semantic_floor`,
`search_min_score`, `search_relative_cut`) tuned against a large, noisy,
cross-session real memory graph — see this repo's own `eval/` ladder
(100–1500 notes). Measured directly against a live ForgetEval case (a
six-node graph with exactly one obviously-relevant note):

```
GET /search?q=What theme does Charlie use?     (default policy)  -> []
GET /search?q=What theme does Charlie use?     (delivery_floor=0) -> [{"score": 0.201, "title": "Charlie switched to dark mode..."}]
```

The single relevant note scores 0.201 — below the 0.22 floor — so the
*default* policy returns nothing at all, even though it is the only
candidate in a six-note graph. Worse, because the adapter's own
supersede/release/purge target-resolution goes through the same `/search`
endpoint, the floor also blinds the adapter's mutation logic: a drift chain
of three sequential `supersede()` calls, run against default policy,
resolved **zero** of its three lookups (every call fell through to "no
hits → just inscribe the new fact"), leaving all three superseded
employers simultaneously recallable — not a supersession bug, a resolution
blackout.

None of the reference adapters have an analogous absolute floor:
`Lethe.recall()` is unconditional top-k nearest-neighbor; Mem0's
`.search(top_k=k)` has no score floor; LangGraph's
`InMemoryStore.search(limit=k)` has no score floor. Running engram's
*default* policy here would not be measuring "does engram forget things" —
it would be measuring "does an orthogonal, differently-scaled confidence
calibration happen to clear its own bar in a 6-note synthetic graph," which
is a different (and, for this benchmark's regime, unfair) question.

So: every `EngramAdapter` PUTs
`delivery_floor = semantic_floor = search_min_score = search_relative_cut
= 0` **and `knee_cliff = null`** on its **own, throwaway** project
directory once at construction, and re-applies it after every `reset()`.
(`knee_cliff` is an option: `null` is off, while `0` is the *harshest*
knee trim there is — every relative drop clears a zero cliff, so every
result gets cut at its largest score drop. The first run of 2026-09-12
zeroed it, which is why its amnesia numbers were low; that run is kept as
`results/2026-09-12-forgeteval-*-run1-kneebug.*`, and the core now refuses
a zero cliff with a teaching error.) The re-apply matters (config lives
inside the tepin store file, so deleting the file for `reset()` also
reverts policy to the store's defaults — confirmed empirically, see
`engram_adapter.py`'s `_apply_policy`). This is scoped to the two
`forgeteval-store*` directories this benchmark creates and never touches
the developer's real graphs (`engram`, `eval`, `home`).

**Default-policy control measurement** (not the headline number — run
separately, before disabling the floor, to quantify the effect):

| suite (n=20 smoke sample) | default policy | floor disabled |
|---|---|---|
| template | 10/20 (50%) | 14/20 (70%) engram, 18/20 (90%) engram-notomb |
| adversarial | 9/20 (45%) | 10/20 (50%) engram, 12/20 (60%) engram-notomb |

(Full numbers below are all measured with the floor disabled.)

## The tombstone-marker leak

`release(query)` maps to engram's soft-evict: `DELETE
/nodes/{id}?tombstone=true&keep_text=false&reason=released`. Per this
repo's own 0.9.0/0.9.2 design, that mints a new `Tombstone` node titled
`"Removed: <victim's title>"` with a body reading `Deleted <type>
"<victim's title>" (id ...).  **Why:** released` — and that marker is a
completely ordinary, searchable node.

`keep_text=false` is meant to mean "the victim's body is not carried
forward." **It does not mean the victim's identity is not carried
forward**: the deletion notice always names the victim's *title*, and this
benchmark's own `inscribe()` sets `title` to the first sentence of the
fact — which, for ForgetEval's short single-sentence facts, is usually the
entire fact. Concretely, releasing `"Session OTP for Liam: 190268."` mints
a tombstone titled `"Removed: Session OTP for Liam: 190268."` — the code
that was just "forgotten" is sitting in the marker's own title, in plain
text, forever (Tombstones never decay — see this repo's `describe_ontology`
and the 0.9.0 changelog: "durability stable ... don't resurrect it"). With
the delivery floor disabled, that marker is now easily the top hit for
"Any one-time codes for Liam?" and substring-grading counts it as a leak.

This is not a bug in the adapter or in this measurement — it is exactly the
scenario the task anticipated, and exactly why ForgetEval is run twice:

- **`engram`** — the role-blind reader: `recall_texts` returns every hit,
  Tombstone markers included. Every `decay` and `amnesia` case fails purely
  because the marker names the released fact in its own title.
- **`engram-notomb`** — the role-aware reader: `recall_texts` asks the
  server for k non-tombstone hits, isolating the release/purge *mechanism*
  from the marker. `decay` is 0% under one reader and 100% under the other
  (see results below) — the release mechanism itself works; the marker's
  title is what a role-blind reader counts.

Neither variant is hidden behind the other. `engram-notomb` is a real,
supportable configuration — and since 0.9.4 a cheap one on any ontology:
every hit that plays the tombstone role carries `tombstone: true`, so a
caller no longer needs to know the type's name to skip or flag it. It is
still a *configuration a caller has to choose to apply*; a role-blind
`GET /search` after a release returns the marker, by design.

## Results

Measured 2026-09-12 against a locally running engram machine core
(`GET /health` → `{"status":"ok","version":"0.9.0", ...}` — the version
string reads 0.9.0 because the tag stamp only happens in CI; the running
binary is built from current source), lethe clone at commit
`b6053b7bdacc78a91b9ea4bb25f32edad278c495`, seed=42, distractors=4.
Receipts: `eval/results/2026-09-12-forgeteval-template.json` /
`...-adversarial.json` (raw per-case outcomes) with matching `.log` files.
This is the **second** run of the day: the first (kept as
`...-run1-kneebug.*`) had two adapter bugs — `knee_cliff` zeroed instead
of nulled (see "Config deviation"), and the role-aware reader filtering
tombstones *after* taking top-k so markers ate the slots. The `lethe`
section is carried over from the first run unchanged (in-process, same
seed, unaffected by either bug).

**The families are reported apart and never summed.** They do not measure
one thing: supersession/drift/purge are edits engram makes the same way
the benchmark expects; decay and amnesia are the benchmark's names for an
explicit *release* (nothing is time-based — engram has no time decay and
will not get one), which maps to engram's tombstoned delete, whose marker
deliberately stays findable. The two readers make that visible instead
of averaging it away.

### Template suite (200 cases per family)

| family | engram (role-blind) | engram-notomb (role-aware) | lethe (reference) |
|---|---|---|---|
| supersession | 200 (100%) | 200 (100%) | 200 (100%) |
| drift | 198 (99%) | 198 (99%) | 198 (99%) |
| purge | 200 (100%) | 200 (100%) | 200 (100%) |
| decay | 0 (0%) | 200 (100%) | 200 (100%) |
| amnesia | 0 (0%) | 185 (92%) | 195 (98%) |

N/A count: 0 for all three. Role-blind decay and amnesia are 0 **by
design**: with the knee trim actually off, the release marker ("Removed:
<fact>") is the top hit for every follow-up query, and substring grading
counts it. Role-aware decay is 100%: the release mechanism resolves and
evicts exactly the target's notes. The role-aware amnesia residual is ten
cases where a generic "Tell me about people." over a five-note graph does
not rank the surviving peer's fact in the top-k — a ranking gap on a
query register engram is not built for, not a forgetting bug (every
release in the failing cases checked evicted the right set).

### Adversarial suite (385 cases: 10 attack categories)

| category | n | engram (role-blind) | engram-notomb (role-aware) | lethe (reference) |
|---|---|---|---|---|
| substring_trap | 36 | 20 (56%) | 29 (81%) | 33 (92%) |
| prefix_collision | 39 | 30 (77%) | 30 (77%) | 32 (82%) |
| paraphrase_supersession | 38 | 29 (76%) | 29 (76%) | 31 (82%) |
| negation_trap | 40 | 19 (48%) | 38 (95%) | 38 (95%) |
| temporal_qualifier | 37 | 37 (100%) | 37 (100%) | 37 (100%) |
| shared_attribute | 40 | 3 (8%) | 38 (95%) | 35 (88%) |
| compound_fact | 40 | 0 (0%) | 0 (0%) | 0 (0%) |
| identifier_obfuscation | 38 | 2 (5%) | 2 (5%) | 2 (5%) |
| cross_lingual_identifier | 38 | 1 (3%) | 1 (3%) | 0 (0%) |
| recursive_supersession | 39 | 36 (92%) | 36 (92%) | 36 (92%) |
| all | 385 | 177 (46%) | 240 (62%) | 244 (63%) |

N/A count: 0 for all three. The release-based categories (`negation_trap`,
`shared_attribute`, `substring_trap`) are where the two readers part, for
the same marker reason as decay. `compound_fact`, `identifier_obfuscation`
and `cross_lingual_identifier` are hard for every adapter under test,
lethe included; both systems embed with the same model, so the one-case
differences there are noise at 36–40 cases per category.

Timing is deliberately not reported: the engram variants ran concurrently
against one shared core while other benchmarks used the machine, so wall
clock says nothing about the mechanism. Per-operation timings stay in each
receipt's `timing_ms` for anyone who wants them.

## Abstention: what ForgetEval does not measure

Every ForgetEval case — template or adversarial — is graded entirely by
substring checks on facts the case itself inscribed:
`must_contain`/`must_not_contain` never mention anyone the case didn't set
up. That leaves an entire failure mode invisible to the benchmark's own
score: a system that confidently invents an answer about a subject it was
**never told about** would pass every standard ForgetEval case while being
unreliable in exactly the way that matters for a memory system. Symmetrically,
the benchmark has no way to see a system's own "I'm not sure" signal, or
credit it for using one. This section adds both measurements as a layer on
top of the existing suites — **the standard pass/fail numbers above are
completely unchanged**; nothing here alters `must_contain`/`must_not_contain`
scoring, an adapter's target-resolution rules, or ForgetEval's own generator
or scoring code (still unpatched).

### The protocol

For **every** case, after `tc.run(adapter)` completes (setup facts inscribed,
mutations applied, the case's own `final_query` already scored) and *before*
the adapter is reset for the next case, the runner puts three **control
queries** to the same live adapter state — about subjects the case never
mentioned:

1. **Control queries.** Each `GeneratedCase` (template and adversarial share
   the same dataclass) carries a `family` (`supersession` / `decay` /
   `amnesia` / `purge` / `drift`). `run_engram.py`'s `CONTROL_TEMPLATES` maps
   each family to 2–3 question shapes lifted near-verbatim from
   `bench/forgeteval/generate.py`'s own `final_query` f-strings for that
   family:

   | family | control templates |
   |---|---|
   | supersession | `Where does {name} work?` · `What theme does {name} use?` · `What is {name}'s diet?` |
   | decay | `Any one-time codes for {name}?` · `What are {name}'s travel plans?` · `Tell me about {name}.` |
   | amnesia | `What does {name} like to eat?` · `What has {name} accomplished?` · `Tell me about {name}.` |
   | purge | `Tell me about {name}.` · `What is the API key for {name}?` · `Show me customer data for {name}.` |
   | drift | `Where does {name} work?` · `What is {name}'s address?` · `What is {name}'s favorite color?` |

   For each case, a `random.Random(f"{seed}:{case.id}:controls")` instance
   (deterministic given `--seed` and the case id, independent of
   `PYTHONHASHSEED`) samples 3 names from `bench.forgeteval.generate.NAMES`
   **minus every name appearing anywhere in that case's `setup_facts`,
   mutations, or `final_query`** (regex word-boundary scan), and 3 templates
   from that family's pool (with replacement — some pools have only 2
   entries), then formats `template.format(name=name)`.

2. **Answered vs. declined.** An adapter *answers* a control iff it returns
   at least one text **and does not decline**:
   - `engram-mcp` declines when *that specific call's* MCP verdict was
     `weak` or `none` — even if it still returned nearest-candidate texts
     (the 0.8.1 "likely not in memory, delivered anyway" behavior; see this
     repo's own chronicle). This is read straight off
     `stats["mcp_confidence_log"][-1]` immediately after the call.
   - `lethe` never declines (`Lethe.recall()` is unconditional top-k) unless
     the store itself is empty for that query.
   - `grep` declines exactly when the query shares zero content words with
     every stored fact (`recall_texts` returns `[]`).
   - `engram`/`engram-notomb` (REST) have no verdict signal available to
     `recall_texts` at all, so for them "declined" collapses to "returned
     nothing" — the same rule as lethe/grep.

3. **FP** = `controls_answered / controls_asked`, per bucket and overall.
   **Lower is better** — but see the warning below.

4. **Hedge** = share of the case's own real `final_query` calls (one per
   case, already scored by the standard suite) where `engram-mcp`'s verdict
   was `weak` or `none`. This is engram-mcp's *cost side*: a system that
   hedges on every real question would show FP≈0 for a trivial reason. For
   `lethe`/`grep`/`engram`/`engram-notomb`, hedge is **0 by construction** —
   none of them carry a verdict signal, so the receipts say so explicitly in
   `hedge.note` rather than reporting a number that would look measured.

5. **Absence signal** (engram-mcp only, informational, no `must_contain`
   implications) = on template `decay`/`amnesia` cases and any case (either
   suite) whose mutations include a `release` op, the share where the case's
   final-query verdict was `weak`/`none` **or every hit MCP returned for that
   query was tombstone-flagged** (`all_tombstone` in the log, computed from
   the *pre-filter* hit list — the role-aware texts returned to scoring have
   already had tombstones removed, so this has to be captured at the MCP
   call site, before that filtering happens). Reads as "the system forgot,
   and its own signal said so" — a property no other adapter here exposes
   (0/0, not 0%, for the rest — there is nothing to compute).

### Why FP is never reported alone

**A mute system wins this metric for free.** `grep` declines whenever a
query's content words are unseen — which happens to be true of nearly every
well-formed control query in this design, so `grep`'s FP is trivially low
*without engram's calibration doing any work*. Reporting FP by itself would
make "answers nothing" look like the best possible memory system. Every
table below prints FP **next to** the case's own pass rate: a low FP paired
with a high pass rate is the only combination that means anything (declining
selectively — never on the real question, always on the control) — that is
what `engram-mcp`'s hedge number is for. `merge_receipts.py`'s summary table
enforces this layout; there is no code path that prints FP without recall
alongside it.

### Separation: a threshold-free companion to fp/hedge

FP and hedge are both read off ONE FIXED verdict line — `engram-mcp`'s own
`weak`/`none` cut, calibrated (`policy.weak_evidence_top`) against this
repo's large, noisy, cross-session real memory graph, not against a
six-to-ten-note synthetic ForgetEval case. A high hedge paired with a low FP
is indistinguishable, read alone, from "the system is simply mute here" —
which would be the wrong conclusion if the underlying scores actually
separate real questions from control questions cleanly and the fixed line
is just drawn in the wrong place for a graph this small. `separation`
answers that question directly, without reference to any threshold at all:
for every case's real `final_query` call (the *answerable*/positive class)
and every control probe (the negative class), the runner captures the
adapter's own NATIVE top score — `engram-mcp`'s raw MCP hit score, `lethe`'s
cosine similarity (read via the same lower-level `Lethe.recall()` call
`LetheAdapter.release()`'s gap-threshold already uses, from a same-file
subclass — nothing in `eval/data/lethe` is touched), `grep`'s overlap count
— then computes the AUC (Mann–Whitney rank-sum, ties=0.5, a missing score
scored as the observed floor rather than dropped) between the two score
populations. An AUC near 1.0 means the populations are cleanly separable on
that adapter's own scale — a decline rule COULD work well here, whatever
fp/hedge says about the one rule actually in use; an AUC near 0.5 means the
scores carry no separating signal at all, and no threshold placement would
ever fix that regardless of where the verdict line is moved. Reported per
bucket and totals as
`separation: {auc, answerable_mean, control_mean, n_answerable, n_control}`
in every receipt; `n/a` for `engram`/`engram-notomb`, which log no native
score to compare. As with the section above, no numbers from an actual run
are asserted here — see the per-run receipts once a full measurement exists.

### Results (2026-09-12, full suites)

Receipts `eval/results/2026-09-12-forgeteval-fp-{template,adversarial}.json`
(+ per-adapter files and `-run.log`), same seed/distractors/scale as the
main run, live core 0.9.4, three control probes per case.

| template (1000 cases, 3000 controls) | pass | fp (controls answered) | hedge | separation AUC | forgets and says so |
|---|---|---|---|---|---|
| engram-mcp | 983 (98%) | **0%** | 99.5% | 0.80 | 400/400 |
| grep | 850 (85%) | 3% | 0 by construction | 0.80 | n/a |
| lethe | 993 (99%) | 100% | 0 by construction | 0.75 | n/a |

| adversarial (385 cases, 1155 controls) | pass | fp | hedge | separation AUC | forgets and says so |
|---|---|---|---|---|---|
| engram-mcp | 252 (65%) | **0%** | 87% | 0.90 | 64/75 |
| grep | 266 (69%) | 4% | 0 by construction | 0.91 | n/a |
| lethe | 244 (63%) | 100% | 0 by construction | 0.94 | n/a |

Per family, engram-mcp's separation is supersession 0.96, drift 0.93,
decay 0.89, purge 0.72, amnesia 0.55; its answerable top scores average
0.17–0.44 against 0.06–0.13 for controls.

**How to read this honestly.** Lethe has no decline rule, so its 100% is a
product choice, not a capability — its own similarity separates real from
control at 0.75–0.94 and would support one. engram's 0% is *not* evidence
of discernment on this register either: with the verdict weak on 99.5% of
real template queries, the line simply never clears on a six-note graph.
The calibrated weak line is fitted from phantom probes over the graph's
own vocabulary, which a per-case graph of six to fifteen short facts cannot
support, so the 0.85 default rules and everything reads weak. What engram
can claim is the separation: without any tuning its scores rank real
questions above never-inscribed ones at 0.80–0.90, so a line fitted on the
graph's actual score population would decline the controls and pass most
real questions. Amnesia (0.55) is the register where that fails — "tell me
about people" scores like a control. This is the same open problem as the
delivery floor on the ladder, in a fourth register; nothing here changes
the product, it prices the gap. Grep's 3–4% comes from shared names, and
its 85% template pass is why fp is never read alone.

Two footnotes. engram-mcp's adversarial pass (252) is above the REST
role-aware run's 240, all of it in `cross_lingual_identifier` (15/38 vs
1/38) — same engine, different transport, unexplained and left as
measured. And an MCP session captures its engine once at creation, so the
adapter re-handshakes after every `reset()`; without that, a session
outlives the store file it was opened on (recorded as an open problem in
the engram graph).

### Commands

```bash
cd eval/forgeteval

# smoke (this repo's own record used --limit 20 per suite)
../data/venv/bin/python3 run_engram.py --adapter engram-mcp --suite template    --limit 20 --project-dir <throwaway-dir-1>
../data/venv/bin/python3 run_engram.py --adapter engram-mcp --suite adversarial --limit 20 --project-dir <throwaway-dir-2>
../data/venv/bin/python3 run_engram.py --adapter grep       --suite template    --limit 20
../data/venv/bin/python3 run_engram.py --adapter grep       --suite adversarial --limit 20
../data/venv/bin/python3 run_engram.py --adapter lethe      --suite template    --limit 20
../data/venv/bin/python3 run_engram.py --adapter lethe      --suite adversarial --limit 20

# full receipts (same shape as the existing engram/engram-notomb/lethe runs)
../data/venv/bin/python3 run_engram.py --adapter engram-mcp --suite template    --scale 200 --project-dir <dir> --out ../results/<date>-forgeteval-template-engram-mcp.json
../data/venv/bin/python3 run_engram.py --adapter grep       --suite template    --scale 200               --out ../results/<date>-forgeteval-template-grep.json
../data/venv/bin/python3 run_engram.py --adapter engram-mcp --suite adversarial --project-dir <dir>        --out ../results/<date>-forgeteval-adversarial-engram-mcp.json
../data/venv/bin/python3 run_engram.py --adapter grep       --suite adversarial                            --out ../results/<date>-forgeteval-adversarial-grep.json

python3 merge_receipts.py template    ../results/<date>-forgeteval-template.json    ../results/<date>-forgeteval-template-*.json
python3 merge_receipts.py adversarial ../results/<date>-forgeteval-adversarial.json ../results/<date>-forgeteval-adversarial-*.json
```

`--project-dir` for `engram-mcp` follows the same rule as `engram`/
`engram-notomb`: a throwaway directory outside any registered project, never
`engram`/`eval`/`home`. `grep` and `lethe` need no project directory — `grep`
has no daemon at all. No numbers from an actual run are recorded in this
section; see the per-run receipts in `eval/results/` for measured values
once a full (non-smoke) run has been made.

## Protocol deviations (summary)

1. **Delivery floor disabled** (four floor knobs zeroed, `knee_cliff`
   nulled) on the two throwaway forgeteval projects — see above; the
   default-policy control is kept there too.
2. **Purge/release resolution mirrors Lethe specifically** rather than a
   majority vote of the three disagreeing reference adapters.
3. **Two readers, both run to completion** — the role-blind and
   role-aware results are two adapters, not a flag, so the marker effect is
   visible as a delta rather than folded away.
4. Node type: **`Insight`** (`describe_ontology`: "I realized something
   non-obvious"; default durability `episodic`) is the closest fit engram's
   9-type reasoning-memory ontology has for an arbitrary personal/factual
   statement. engram's ontology is a *decision/reasoning* memory for coding
   agents, not a general personal-fact store, so this is an approximation,
   stated as such.
5. **`engram-mcp` re-handshakes its MCP session on every `reset()`.** An MCP
   session's engine handle is bound once, at session creation
   (`Engram::for_project` runs inside the per-session factory
   `streamable_http_service_for` hands to `StreamableHttpService`), and is
   never rebound afterward. `reset()` deletes the store file out from under
   the daemon so a REST call reopens a fresh engine on next touch — correct
   for REST, which resolves its engine fresh per request, but fatal for a
   long-lived MCP session: keeping one `mcp-session-id` across a `reset()`
   silently served every subsequent case the FIRST case's frozen data
   (found while smoke-testing this adapter — every case after the first
   failed until this was fixed). Nothing about normal engram usage triggers
   this — no real client deletes a live project's store file out from under
   an open bridge session — it is purely an artifact of ForgetEval's
   `reset()` protocol. `EngramMCPAdapter.reset()` now clears the cached
   session id so the next `recall_texts` call re-handshakes.
6. **`engram-mcp`'s hedge is not tunable by this adapter's own policy
   override.** `disable_delivery_floor` zeros `delivery_floor` /
   `semantic_floor` / `search_min_score` / `search_relative_cut` and nulls
   `knee_cliff` — none of which affect `search_confidence`'s strong/weak
   split, which instead compares the top hit's score against
   `policy.weak_evidence_top` (default 0.85, untouched here). That knob is
   calibrated against this repo's own large, noisy, cross-session real
   memory graph (the `eval/` ladder), the same category of mismatch the
   "Config deviation" section above measured for `delivery_floor` against a
   six-node synthetic graph. The task scoped this adapter to mirror
   `engram-notomb`'s policy exactly, so `weak_evidence_top` is left as
   shipped — meaning the hedge numbers this adapter produces should be read
   as "how often the real-graph confidence calibration clears its bar on a
   tiny per-case ForgetEval graph," not as a verdict on engram's confidence
   mechanism in general. Flagged qualitatively here rather than smoothed
   over; no numbers are asserted (see the section above for why).

## Known limitations

- ForgetEval's substring grading is exact and case-insensitive but not
  normalization-aware beyond that; some adversarial categories (identifier
  obfuscation, cross-lingual identifiers) are hard for every adapter under
  test, engram included, and that is visible in the per-category table
  above rather than smoothed over.
- The daemon (`127.0.0.1:8787`) is shared machine-wide and the engram
  variants ran concurrently with other benchmarks, which is why no timing
  is reported: the per-operation numbers in each receipt's `timing_ms` are
  contended and say nothing about the mechanism.
- `mem0` / `langmem` / `cognee` / `amem` reference adapters were not run:
  the task scoped this to engram + engram-notomb + lethe (the only
  zero-external-service reference), to keep the exercise to what installs
  and runs offline in minutes.
- This is a single measurement (one seed, one distractor count) — not a
  ladder. Re-running with different `--seed`/`--distractors` would be
  needed before treating any single percentage as a stable number.
