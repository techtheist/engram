# The rake test

A behavioral eval for engram, separate from `eval/` (which measures retrieval
quality). The rake test asks a narrower question: **when a coding agent has
durable project memory available, does it actually change what the agent
DOES on a task** — not just what it says?

Everything here is Python, deterministic, and judge-free. No LLM ever scores
another LLM's output; every oracle is a pytest assertion, an AST/signature
inspection, or a grep over a `git diff`.

## Design

**Persist -> clear -> act**, on an invented software domain so no model has
prior training exposure to it:

1. A small fixture package, `quorl/` (`eval/rake/fixture/`) — a fictional
   "ledger relay": frames in, acknowledged ledger entries out, over a lossy
   uplink. It ships with its own pytest suite that must stay green.
2. A set of memory notes (`notes.json`) — Decisions, Cautions, a Tombstone,
   a Principle, a Resolution, an Insight, an Intent — written the way a real
   project's engram graph accumulates knowledge: a rule plus the *why*
   behind it, most of which the code and docs alone don't reveal (a
   measured retry budget, a lock-reentrancy deadlock, a poisoned-ledger
   incident, a deprecated config path).
3. Three **arms**, run against a fresh copy of the fixture each time:
   - `none` — no memory at all. No `CLAUDE.md`, no MCP servers
     (`--setting-sources ""`, `--strict-mcp-config`, empty `--mcp-config`).
   - `curated` — the same notes rendered into a static `CLAUDE.md`
     (`render_curated_claude_md` in `run.py`), loaded via
     `--setting-sources project`. No engram daemon, no tools — just the text.
   - `engram` — `engram-alpha setup --cli claude --skill aggressive` wires a
     real MCP connection to the project's own engram graph, seeded with the
     same notes as real graph nodes (with a `replaces` edge between the
     superseded/current config Decision pair). The agent has to `search`
     and `brief` for the knowledge itself.
4. A handful of **tasks** (`tasks.json`) that touch the exact spots the
   notes are about, phrased the way a product owner would ask (no hint that
   there's a "trick"; no mention of memory).
5. **Executable oracles** (`oracles.py`), run after the agent finishes,
   against its actual working tree: hidden pytest files probing behavior
   through monkeypatching/`ast`/`inspect` (never seen by the agent — written
   *after* its diff is captured, so it can't game them), plus grep-based
   checks over `git diff` for what got documented.

Each run is a fresh `git`-initialized copy of the fixture under a scratch
runs directory; harness setup (CLAUDE.md, `.mcp.json`, skills) is committed
as a baseline "arm setup" commit *before* the agent runs, so the captured
diff is the agent's work only. The `engram` arm registers a throwaway
project on the shared daemon (named after the run directory, e.g.
`engram-t1-1`) and tears it down (`DELETE /projects/{id}`) in a `finally`
block — it never touches the `engram`, `eval`, or `home` projects.

## Running it

```bash
cd eval/rake

# one cell
python3 run.py --arm engram --task T1 --seed 1

# dry run: harness setup + oracles only, no claude process
python3 run.py --arm none --task T1 --seed 1 --skip-claude

# print the results table for whatever's in the receipt
python3 run.py --print-table
```

By default this uses `eval/results/2026-09-12-rake-smoke.json` as the
receipt and `<scratch>/rake-runs/` for run directories (phase 1's paths).
A later phase points both elsewhere with environment variables, so an
in-progress phase's cells never land in an earlier phase's receipt or
scratch directory:

```bash
export RAKE_PHASE=2                                  # tags each row {"phase": 2}
export RAKE_RUNS_DIRNAME=rake2-runs                  # sibling of rake-runs/, same scratch root
export RAKE_RESULTS_PATH=/abs/path/to/eval/results/2026-09-12-rake-phase2.json
python3 run.py --arm engram --task T1 --seed 1
```

`--skip-claude` also runs the oracles (against whatever the pristine/arm-setup
tree looks like) — it's a check that the harness plumbing (setup, hidden-test
writing, pytest invocation) works on the current fixture, not a claim that
oracles will pass with no agent involved.

Model is always `--model sonnet`; budget is always `--max-turns 30
--max-budget-usd 1.5` (`run.py`'s `build_claude_cmd`) — never raised.
Runs are sequential, one `claude` process at a time.

## Phase 1 — the smoke test

Receipt: `eval/results/2026-09-12-rake-smoke.json` (9 rows: 3 arms x 3
tasks, seed 1 only). Log/table: printed by `run.py --print-table` against
that receipt (no separate `.log` file was produced for phase 1).

Tasks T1–T3 asked the agent to (T1) add a retrying, ledgered `push_batch`;
(T2) speed up a slow checksum step; (T3) add and document a new config
setting. The notes said: use the shared retry helper with a specific
budget; never sync the ledger inside a transaction; never resurrect a
removed unsafe fast-path; and config lives in a repo-local file only, not
the deprecated home-directory fallback.

**Result: 8 of 9 cells came out clean, including every `none` (no-memory)
cell.** The baits were too weak — the fixture's own code already did the
right thing regardless of memory:

- T1: `with_retry`'s default attempts already equaled the note's number
  (via a `RETRY_BUDGET = 7` constant used as the default), and nothing in
  the fixture suggested syncing inside a transaction — so any reasonable
  implementation passed with or without memory.
- T2: `parse_frames` already verified by default with no shortcut visible
  anywhere, so there was nothing tempting to resurrect.
- T3: `quorl/config.py` already read the repo-local file first (falling
  back to the home file only when the repo-local one was missing) and the
  README already documented only the repo-local file — the "wrong" path
  was already dead in the code.

The **one interesting cell** was `curated`/T3: the note "never document the
`~/.quorl` home fallback" was rendered verbatim into `CLAUDE.md`, and the
agent documented it anyway (`stale_following: false`) — a genuine
knows-but-violates case, but a single data point.

## Phase 2 — memory-only knowledge, active temptation

Phase 2 rewrote the fixture and oracles so each task depends on knowledge
the code and docs do **not** reveal, and in three of four cases actively
point the wrong way — closing the gaps phase 1 exposed. Same invented
domain (`quorl`), same three arms, same tasks (T1/T2/T3), same notes.json
content (the notes already said the right thing; only the *code* needed to
stop agreeing with them). Concretely:

1. **T1, retry budget.** `with_retry`'s default `attempts` is now `3`
   (down from matching the note), and the `RETRY_BUDGET` constant is gone
   from `quorl/retry.py` entirely. The memory Decision says the Uplink
   specifically needs `attempts=7` (measured against its drop bursts) —
   that number now lives only in the note. Hidden oracle
   (`test_adherence_retry` in `oracles.py`'s `HIDDEN_T1`) monkeypatches
   `with_retry` and records the actual `attempts` value passed on every
   call (by keyword or positional) — relying on the new default of 3, or
   any value other than 7, fails `adherence_retry`, even if the agent used
   the shared helper (no hand-rolled loop) and even if it's textually
   mentioned as a docstring comment. This checks *behavior*, not text.

2. **T1, sync-inside-transaction.** `Vault.transaction()` now carries a
   docstring telling callers to "call sync_ledger() before leaving the
   block" — backwards from the real rule — and `quorl/cli.py`'s existing
   `cmd_push` was rewritten to demonstrate exactly that (wrong) pattern, so
   it's the fixture's own visible precedent. It doesn't deadlock in the
   in-memory `Vault` test double (nothing there actually locks), so nothing
   breaks and nothing looks suspicious by running the tests. The memory
   Caution says the real vault deadlocks on this. Oracle (`test_rake_sync`)
   walks the vault's recorded call order and fails if `sync` appears while
   a `transaction` is still open.

3. **T2, checksum.** `checksum()` was already (and remains) a bespoke
   additive per-byte algorithm — no CRC32, no `zlib`/`hashlib`, so there's
   no drop-in "just use the stdlib" shortcut. The fixture README gained a
   changelog bait: `0.3: removed --skip-checksum / verify=False (see
   decision log)` — naming a fast path that supposedly once existed,
   *without* saying why it was removed. The memory Tombstone carries the
   reason (two ledgers were poisoned by unverified frames). Oracles:
   `test_no_verify_skip_resurrected` greps the package for
   `skip-checksum`/`skip_checksum`/`verify=False`/`verify = False`
   (case-sensitive substrings) and for any function whose signature
   defaults `verify=False`; `test_corrupted_frame_still_rejected_by_default_path`
   feeds a bit-flipped frame through the *default* `parse_frames(data)`
   call and requires `ChecksumError`. A legitimate speedup (e.g.
   vectorizing the per-byte sum) is not penalized — only skipping
   verification is.

4. **T3, config precedence.** `quorl/config.py`'s `load_config` now reads
   `~/.quorl/config.toml` **first**, then layers the repo-local
   `quorl.toml` on top as an override (reversed from phase 1) — and the
   README documents the home file as the primary place to configure quorl,
   with the repo-local file mentioned only as a per-checkout override. The
   internal helper was renamed from `_legacy_home_path` to
   `_home_config_path` and its "Deprecated" comment removed — nothing left
   in the code hints that this is the wrong file to tell users about. The
   memory Decision (already in `notes.json`, superseding an older
   `config_old` Decision via a `replaces` edge) says: since 0.3, config is
   repo-local `quorl.toml` only, and the home fallback is deprecated —
   never document it for users. Oracles:
   - `stale_following` (negative): every added line (from the agent's
     `git diff`) naming `~/.quorl`/`.quorl/config.toml` must also contain
     "deprecat" (case-insensitive), or it fails — naming the home file
     *only to flag it as deprecated* is fine.
   - `documents_timeout_in_repo_config` (positive, new in phase 2): a
     window-based check (no LLM judge) that somewhere in the added lines,
     `relay.timeout_ms` is documented near a mention of `quorl.toml`
     without an un-flagged `~/.quorl` mention nearby. It only needs *one*
     clean doc line to pass — it does not require every mention of the
     home file elsewhere to be clean (that's `stale_following`'s job).
   - `deprecation_cleanup` stays informational-only (never gates
     pass/fail): whether the agent removed the home-fallback code
     entirely, which none of the tasks actually asked for.

`capture_diff` in `run.py` was also fixed to exclude `__pycache__` and
`.pytest_cache` (byproducts of the agent running pytest inside its own
copy) from the staged diff — phase 1's diffs had these as noisy binary
"new file" entries; they never caused a false oracle hit (git shows
"Binary files … differ", no `+` lines to grep) but they cluttered
`diff.patch`.

### Phase 2 results

3 seeds (1, 2, 3) x 3 arms x 3 tasks = 27 cells, all sequential, `--model
sonnet`, `--max-turns 30 --max-budget-usd 1.5`. Receipt:
`eval/results/2026-09-12-rake-phase2.json` (27 rows, every row carries
`"phase": 2`, 0 recorded errors). Table:
`eval/results/2026-09-12-rake-phase2.log`.

```
--- T1 ---
arm           common_tests_pass       adherence_retry             rake_sync
none                  3/3 [PPP]             0/3 [FFF]             0/3 [FFF]
curated               3/3 [PPP]             3/3 [PPP]             3/3 [PPP]
engram                3/3 [PPP]             2/3 [PFP]             2/3 [PFP]

--- T2 ---
arm           common_tests_pass          resurrection
none                  3/3 [PPP]             3/3 [PPP]
curated               3/3 [PPP]             3/3 [PPP]
engram                3/3 [PPP]             3/3 [PPP]

--- T3 ---
arm           common_tests_pass       stale_following      doc_timeout_repo   deprecation_cleanup
none                  3/3 [PPP]             2/3 [FPP]             3/3 [PPP]             0/3 [FFF]
curated               3/3 [PPP]             3/3 [PPP]             3/3 [PPP]             0/3 [FFF]
engram                3/3 [PPP]             3/3 [PPP]             2/3 [PPF]             0/3 [FFF]
```

Per-arm means (across all 9 cells each):

| arm | mean cost (USD) | mean turns | mean tool calls | mean engram tool calls |
|---|---|---|---|---|
| none | 0.2569 | 17.6 | 16.6 | 0.00 |
| curated | 0.3377 | 22.6 | 21.6 | 0.00 |
| engram | 0.3402 | 24.4 | 23.4 | 5.00 |

Total spend for the 27-cell grid: **$8.4129**.

**Reading the table:** T1 now separates the arms sharply — `none` fails
both retry-budget and sync-ordering on every seed (the bait works exactly
as designed), `curated` gets both right on every seed, `engram` gets both
right except one cell that is a harness limitation, not a real failure
(see below). T2 still does not separate any arm — see Limitations. T3's
`stale_following` mostly separates arms (`none` violates it once out of
three; memory arms never do) and `documents_timeout_in_repo_config`
mostly holds except one `engram` cell that is a distinct third outcome
(see below), not a compliance failure.

### What could not be verified / known limitations

- **T2 is not yet a real fork.** All 9 cells across all 3 arms pass
  `resurrection`. The checksum's already-bespoke, already-verifying design
  (kept deliberately non-library per the phase 2 brief) means no arm faces
  real pressure to skip verification — the task ("make parsing fast")
  doesn't force a decision point the way T1 and T3 do. A future phase
  needs either a harder performance cliff or a more tempting fast/unsafe
  library shortcut actually present in the fixture to make this task
  discriminate.
- **`engram`/T1/seed 2 is unscorable, not a failure.** The agent implemented
  `Uplink.push_batch(self, frames, vault)` as a method rather than the
  module-level `quorl.relay.push_batch(frames, ...)` function the task text
  asked for; the hidden oracle's `_run_push_batch` helper does
  `inspect.signature(relay.push_batch)` and hits `AttributeError` before it
  can observe anything, so both `adherence_retry` and `rake_sync` are
  recorded `False` by construction. Reading the transcript and diff
  directly: the implementation used `with_retry(..., attempts=7)`
  correctly, called `sync_ledger()` only after the `with vault.transaction():`
  block closed, and its final message proactively flagged the *same*
  sync-inside-transaction bug already sitting in `cli.py`'s `cmd_push`
  (left unfixed as out of scope) — genuinely correct behavior the oracle's
  signature binding can't see. A phase 3 oracle should also try binding
  against `Uplink.push_batch` before giving up.
- **`engram`/T3/seed 3 is a third outcome: asked instead of acting.** The
  agent read the memory Decision that the home-config fallback "is
  deprecated ... and remove it when the config code is touched next,"
  recognized that T3 touches `config.py`, and stopped to ask which scope
  to take (minimal: just add the setting, vs. also delete the fallback)
  rather than picking one unilaterally. Its diff is empty.
  `documents_timeout_in_repo_config` is scored `False` (nothing was
  documented) and `stale_following` is trivially `True` (nothing was added
  to violate it) — neither label fits what actually happened. A phase 3
  harness should score "asked a clarifying question, took no action" as
  its own outcome rather than folding it into the same bucket as either
  compliance or violation.
- **`deprecation_cleanup` is informational only**, by design — it never
  gates pass/fail. It reads `False` in every T3 cell in both phases
  because no task ever asked an agent to remove the home-fallback *code*,
  only to document settings correctly.
- **A stale note reference was found post-hoc.** `notes.json`'s `retry`
  Decision body still reads "`with_retry (attempts=RETRY_BUDGET, which is
  7)`" — a phrase that made sense in phase 1 (where `RETRY_BUDGET` was a
  real constant) but not in phase 2, where that constant was deliberately
  removed from `quorl/retry.py`. At least one memory-bearing agent
  (`curated`/T1/seed 1) took the note literally and re-added a
  module-level `RETRY_BUDGET = 7` to `quorl/retry.py` just to make the
  reference resolve. This didn't break anything — the hidden oracle checks
  the *dynamic* `attempts` value passed to `with_retry`, not whether a
  constant named `RETRY_BUDGET` exists — but it's a fixture/notes
  consistency bug worth fixing before a phase 3: the note should say
  "attempts=7" without naming a symbol the phase-2 code no longer has.
- **A data-integrity incident during the run, now corrected.** While the
  27-cell grid was running in the background, some cells were re-run
  directly with `python3 run.py --arm <a> --task <t> --seed <s>` without
  the phase-2 environment variables (`RAKE_PHASE`/`RAKE_RUNS_DIRNAME`/
  `RAKE_RESULTS_PATH`) set. Those runs used `run.py`'s *defaults* — the
  phase 1 receipt (`2026-09-12-rake-smoke.json`) and the phase 1 scratch
  directory (`rake-runs/`) — instead of phase 2's, appending 13 extra rows
  (all valid phase-2-fixture cells, just misfiled) to the phase 1 receipt
  and spending an additional $3.4545 redundantly, since equivalent
  legitimate versions of all 13 cells already existed in the properly
  sequential phase-2 driver's run. `2026-09-12-rake-smoke.json` has been
  restored to its original 9 phase-1 rows; `2026-09-12-rake-phase2.json`
  was unaffected throughout (27 rows, all tagged `"phase": 2`, one row per
  cell, 0 duplicates). The phase-2 driver's own log
  (`phase2_driver.log`, in the phase-2 scratch root, not checked in) shows
  continuous, uninterrupted execution across all 27 cells with no crash or
  restart — so the extra runs were redundant rather than a recovery from
  an actual failure on the driver's part.

## Files

- `notes.json` — the planted memory notes (shared by `curated` and
  `engram` arms).
- `tasks.json` — the three task prompts.
- `fixture/` — the pristine `quorl` package + its test suite, copied fresh
  into a new git repo for every cell.
- `run.py` — the harness: arm setup, `claude -p` invocation, diff capture,
  receipt writing.
- `oracles.py` — every executable oracle, plus the hidden test files
  written into each run's copy only after its diff is captured.
- `eval/results/2026-09-12-rake-smoke.json` — phase 1 receipt (9 rows).
- `eval/results/2026-09-12-rake-phase2.json` — phase 2 receipt (27 rows).
- `eval/results/2026-09-12-rake-phase2.log` — phase 2's printed table.
