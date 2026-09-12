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
Rust code touched — and runs both suites against it.

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
  deviation).
- `run_engram.py` — wraps the upstream `bench/forgeteval/{generate,
  adversarial}.py` generators and `tests.py`'s `TestCase.run` /
  `GeneratedCase.run` scoring loop; nothing inside the `lethe` clone is
  patched.
- `merge_receipts.py` — combines the three per-adapter JSON receipts for one
  suite into the single `eval/results/<date>-forgeteval-<suite>.json` file.

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

### Timing

`engram` and `engram-notomb` ran **concurrently** against one shared core
(two HTTP clients, one daemon), so per-case cost is inflated by contention;
`lethe` ran alone, in-process.

| run | wall | ms/case |
|---|---|---|
| template — engram | 575.0s | 575 |
| template — engram-notomb | 569.3s | 569 |
| template — lethe | 106.0s | 106 |
| adversarial — engram | 149.6s | 389 |
| adversarial — engram-notomb | 149.1s | 387 |
| adversarial — lethe | 23.2s | 60 |

Per-operation timings are in each receipt's `timing_ms`; an uncontended
smoke earlier in the day measured `reset()` at ~90–100 ms and a full
inscribe→search round trip under 400 ms/case.

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

## Known limitations

- ForgetEval's substring grading is exact and case-insensitive but not
  normalization-aware beyond that; some adversarial categories (identifier
  obfuscation, cross-lingual identifiers) are hard for every adapter under
  test, engram included, and that is visible in the per-category table
  above rather than smoothed over.
- The daemon (`127.0.0.1:8787`) is shared machine-wide; running two engram
  variants concurrently measurably slows both down (see timing) — the
  timing numbers below reflect whatever concurrency was actually used for
  this record (noted per run).
- `mem0` / `langmem` / `cognee` / `amem` reference adapters were not run:
  the task scoped this to engram + engram-notomb + lethe (the only
  zero-external-service reference), to keep the exercise to what installs
  and runs offline in minutes.
- This is a single measurement (one seed, one distractor count) — not a
  ladder. Re-running with different `--seed`/`--distractors` would be
  needed before treating any single percentage as a stable number.
