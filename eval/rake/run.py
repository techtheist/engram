#!/usr/bin/env python3
"""The rake test: does planted reasoning memory change what a coding agent DOES.

Usage:
    python3 run.py --arm none --task T1 --seed 1
    python3 run.py --arm engram --task T1 --seed 1 --skip-claude   # dry run

Everything happens inside a fresh temp copy of eval/rake/fixture under
RAKE_RUNS_DIR/<arm>-<task>-<seed>/. Never touches crates/, frontend/, or the
engram/eval/home projects registered on the running daemon.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

import requests

import oracles

REPO_ROOT = Path("/Users/techtheist/IdeaProjects/engram")
RAKE_DIR = REPO_ROOT / "eval" / "rake"
FIXTURE_DIR = RAKE_DIR / "fixture"
NOTES_PATH = RAKE_DIR / "notes.json"
TASKS_PATH = RAKE_DIR / "tasks.json"

# Phase is env-driven so this file stays the same script across phases: phase
# 1's defaults (rake-runs/, rake-smoke.json, phase omitted from the row) are
# unchanged unless RAKE_PHASE/RAKE_RESULTS_PATH/RAKE_RUNS_DIRNAME are set.
RAKE_PHASE = int(os.environ.get("RAKE_PHASE", "1"))
RESULTS_PATH = Path(
    os.environ.get(
        "RAKE_RESULTS_PATH",
        str(REPO_ROOT / "eval" / "results" / "2026-09-12-rake-smoke.json"),
    )
)

SCRATCH_ROOT = Path(
    "/private/tmp/claude-501/-Users-techtheist-IdeaProjects-engram/"
    "beed9258-835e-40e3-953c-e90b19cf4f7a/scratchpad"
)
RUNS_DIR = SCRATCH_ROOT / os.environ.get("RAKE_RUNS_DIRNAME", "rake-runs")
VENV_PYTHON = REPO_ROOT / "eval" / "data" / "venv-rake" / "bin" / "python3"

DAEMON = "http://127.0.0.1:8787"
GIT_ENV = {
    **os.environ,
    "GIT_AUTHOR_NAME": "rake",
    "GIT_AUTHOR_EMAIL": "rake@test.local",
    "GIT_COMMITTER_NAME": "rake",
    "GIT_COMMITTER_EMAIL": "rake@test.local",
}

DURABILITY_BY_TYPE = {
    "Decision": "stable",
    "Caution": "stable",
    "Principle": "stable",
    "Tombstone": "stable",
    "Problem": "episodic",
    "Resolution": "episodic",
    "Insight": "episodic",
    "Intent": "volatile",
}

COMMON_ALLOWED_TOOLS = "Read,Edit,Write,Glob,Grep,Bash(python3*),Bash(pytest*),Bash(python*)"
ENGRAM_ALLOWED_TOOLS = COMMON_ALLOWED_TOOLS + ",mcp__engram__*"

CLAUDE_BIN = shutil.which("claude") or str(Path.home() / ".local" / "bin" / "claude")
ENGRAM_ALPHA_BIN = shutil.which("engram-alpha") or str(Path.home() / ".local" / "bin" / "engram-alpha")


def load_json(path: Path):
    with open(path) as f:
        return json.load(f)


def run_dir_for(arm: str, task: str, seed: int) -> Path:
    return RUNS_DIR / f"{arm}-{task}-{seed}"


def prepare_copy(run_dir: Path) -> None:
    if run_dir.exists():
        shutil.rmtree(run_dir)
    run_dir.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(FIXTURE_DIR, run_dir)
    subprocess.run(["git", "init", "-q"], cwd=run_dir, check=True, env=GIT_ENV)
    subprocess.run(["git", "add", "-A"], cwd=run_dir, check=True, env=GIT_ENV)
    subprocess.run(
        ["git", "commit", "-q", "-m", "pristine fixture"],
        cwd=run_dir,
        check=True,
        env=GIT_ENV,
    )


def render_curated_claude_md(notes: list[dict]) -> str:
    by_key = {n["key"]: n for n in notes}
    old_to_new = {}  # old_key -> new_key
    new_to_old = {}  # new_key -> old_key
    for n in notes:
        if "replaces" in n:
            old_to_new[n["replaces"]] = n["key"]
            new_to_old[n["key"]] = n["replaces"]

    skip_keys = set(old_to_new.keys())  # old ones rendered inline, not standalone

    def bullet(n: dict) -> str:
        return f"- **{n['title']}** — {n['body']}"

    lines = [
        "# Project memory (curated)",
        "",
        "The following is prior team knowledge about this repository. Treat it as",
        "settled context: decisions and cautions below are already made and should",
        "guide how you implement new work.",
        "",
    ]

    decisions = [n for n in notes if n["type"] == "Decision" and n["key"] not in skip_keys]
    if decisions:
        lines.append("## Decisions")
        for n in decisions:
            if n["key"] in new_to_old:
                old = by_key[new_to_old[n["key"]]]
                lines.append(
                    f"- **{n['title']} (current)** — {n['body']} "
                    f"— supersedes: ~~{old['title']}~~"
                )
            else:
                lines.append(bullet(n))
        lines.append("")

    cautions = [n for n in notes if n["type"] == "Caution"]
    if cautions:
        lines.append("## Cautions")
        for n in cautions:
            lines.append(bullet(n))
        lines.append("")

    principles = [n for n in notes if n["type"] == "Principle"]
    if principles:
        lines.append("## Principles")
        for n in principles:
            lines.append(bullet(n))
        lines.append("")

    resolutions = [n for n in notes if n["type"] in ("Problem", "Resolution")]
    if resolutions:
        lines.append("## Resolved problems")
        for n in resolutions:
            lines.append(bullet(n))
        lines.append("")

    insights = [n for n in notes if n["type"] == "Insight"]
    if insights:
        lines.append("## Insights")
        for n in insights:
            lines.append(bullet(n))
        lines.append("")

    intents = [n for n in notes if n["type"] == "Intent"]
    if intents:
        lines.append("## Intents")
        for n in intents:
            lines.append(bullet(n))
        lines.append("")

    tombstones = [n for n in notes if n["type"] == "Tombstone"]
    if tombstones:
        lines.append("## Removed (do not re-add)")
        for n in tombstones:
            lines.append(f"- **{n['title']}** — {n['body']}")
        lines.append("")

    return "\n".join(lines)


def setup_arm_none(run_dir: Path) -> dict:
    return {}


def setup_arm_curated(run_dir: Path, notes: list[dict]) -> dict:
    md = render_curated_claude_md(notes)
    (run_dir / "CLAUDE.md").write_text(md)
    return {}


def setup_arm_engram(run_dir: Path, notes: list[dict], session: requests.Session) -> dict:
    proc = subprocess.run(
        [ENGRAM_ALPHA_BIN, "setup", "--cli", "claude", "--skill", "aggressive"],
        cwd=run_dir,
        capture_output=True,
        text=True,
        timeout=30,
    )
    setup_stdout = proc.stdout
    if proc.returncode != 0:
        raise RuntimeError(f"engram-alpha setup failed: {proc.stdout}\n{proc.stderr}")

    project_sel = run_dir.name.lower()  # daemon slugifies the dir basename

    r = session.get(
        f"{DAEMON}/brief",
        params={"project": str(run_dir), "max_chars": 1},
        timeout=30,
    )
    r.raise_for_status()

    projects = session.get(f"{DAEMON}/projects", timeout=10).json()
    match = [p for p in projects if p.get("root") == str(run_dir)]
    if not match:
        raise RuntimeError(f"project for {run_dir} not found in registry after setup+brief")
    project_id = match[0]["id"]
    P = f"{DAEMON}/projects/{project_id}"

    ids = {}
    for n in notes:
        body = {
            "type": n["type"],
            "title": n["title"],
            "body": n["body"],
            "durability": DURABILITY_BY_TYPE[n["type"]],
            "source": "user",
            "tags": n.get("tags", []),
        }
        r = session.post(P + "/nodes", json=body, timeout=15)
        r.raise_for_status()
        ids[n["key"]] = r.json()["id"]

    for n in notes:
        if "replaces" in n:
            r = session.post(
                P + "/edges",
                json={
                    "type": "replaces",
                    "from_id": ids[n["key"]],
                    "to_id": ids[n["replaces"]],
                    "source": "user",
                },
                timeout=15,
            )
            r.raise_for_status()

    r = session.get(P + "/search", params={"q": "sync_ledger inside a transaction deadlock"}, timeout=15)
    hits = r.json()
    caution_found = any(h["type"] == "Caution" and "sync_ledger" in h["title"] for h in hits)
    if not caution_found:
        raise RuntimeError(f"seed verification failed: Caution not found in search hits: {hits}")

    return {
        "project_id": project_id,
        "node_ids": ids,
        "setup_stdout": setup_stdout,
        "seed_search_hits": [h["title"] for h in hits],
    }


def teardown_arm_engram(project_id: str, session: requests.Session) -> None:
    try:
        session.delete(f"{DAEMON}/projects/{project_id}", timeout=10)
    except requests.RequestException:
        pass


def build_claude_cmd(arm: str, task_prompt: str, run_dir: Path) -> list[str]:
    common = [
        CLAUDE_BIN,
        "-p",
        task_prompt,
        "--model",
        "sonnet",
        "--max-turns",
        "30",
        "--max-budget-usd",
        "1.5",
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        "dontAsk",
    ]
    if arm == "none":
        # --bare would be the cleanest "no memory" mode, but it requires
        # ANTHROPIC_API_KEY (keychain/OAuth reads are disabled under --bare)
        # and this machine authenticates via OAuth only. Achieve the same
        # effect through normal auth instead: no CLAUDE.md/.claude/.mcp.json
        # exist in this arm's fixture copy, --setting-sources "" skips even
        # user-level settings, and strict empty MCP config skips all servers.
        return common + [
            "--setting-sources",
            "",
            "--strict-mcp-config",
            "--mcp-config",
            '{"mcpServers":{}}',
            "--allowedTools",
            COMMON_ALLOWED_TOOLS,
        ]
    if arm == "curated":
        return common + [
            "--setting-sources",
            "project",
            "--strict-mcp-config",
            "--mcp-config",
            '{"mcpServers":{}}',
            "--allowedTools",
            COMMON_ALLOWED_TOOLS,
        ]
    if arm == "engram":
        return common + [
            "--setting-sources",
            "project",
            "--strict-mcp-config",
            "--mcp-config",
            str(run_dir / ".mcp.json"),
            "--allowedTools",
            ENGRAM_ALLOWED_TOOLS,
        ]
    raise ValueError(arm)


def run_claude(arm: str, task_prompt: str, run_dir: Path) -> dict:
    cmd = build_claude_cmd(arm, task_prompt, run_dir)
    permission_mode_fallback_used = False
    env = {**os.environ, "CLAUDE_PROJECT_DIR": str(run_dir)}

    transcript_path = run_dir / "transcript.jsonl"
    t0 = time.time()
    proc = subprocess.run(
        cmd,
        cwd=str(run_dir),
        capture_output=True,
        text=True,
        env=env,
        timeout=900,
    )
    wall_s = time.time() - t0

    stdout = proc.stdout
    if "dontAsk" in " ".join(cmd) and (
        "invalid" in (proc.stderr or "").lower() or "dontask" in (proc.stderr or "").lower()
    ) and proc.returncode != 0:
        permission_mode_fallback_used = True
        cmd2 = [("acceptEdits" if c == "dontAsk" else c) for c in cmd]
        t0 = time.time()
        proc = subprocess.run(
            cmd2, cwd=str(run_dir), capture_output=True, text=True, env=env, timeout=900
        )
        wall_s = time.time() - t0
        stdout = proc.stdout

    transcript_path.write_text(stdout)

    events = []
    for line in stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            continue

    tool_calls: dict[str, int] = {}
    engram_calls: dict[str, int] = {}
    final_message_text = ""
    result_event = None
    for ev in events:
        if ev.get("type") == "assistant":
            msg = ev.get("message", {})
            for block in msg.get("content", []) or []:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    name = block.get("name", "?")
                    tool_calls[name] = tool_calls.get(name, 0) + 1
                    if name.startswith("mcp__engram__"):
                        engram_calls[name] = engram_calls.get(name, 0) + 1
                if isinstance(block, dict) and block.get("type") == "text":
                    final_message_text = block.get("text", final_message_text)
        if ev.get("type") == "result":
            result_event = ev

    return {
        "cmd": cmd,
        "returncode": proc.returncode,
        "stderr_tail": (proc.stderr or "")[-2000:],
        "wall_s": wall_s,
        "permission_mode_fallback_used": permission_mode_fallback_used,
        "tool_calls": tool_calls,
        "engram_calls": engram_calls,
        "final_message_text": final_message_text,
        "result_event": result_event,
        "num_events": len(events),
        "hit_max_turns_or_error": proc.returncode != 0,
    }


HARNESS_ARTIFACTS = (
    "transcript.jsonl",
    "diff.patch",
    "**/__pycache__",
    ".pytest_cache",
)


def capture_diff(run_dir: Path) -> str:
    """Diff the agent's actual working-tree changes only.

    Excludes harness artifacts (transcript.jsonl, diff.patch) written into
    run_dir after the fact — transcript.jsonl in particular quotes ORIGINAL
    file contents inside tool_result JSON (e.g. Read of the pristine
    config.py, which still contains the legacy ~/.quorl fallback in its
    source comments), so staging it would make oracle grepping the diff see
    false "the agent documented/extended the fallback" hits that are really
    just the transcript recording a Read of pre-existing code. Also excludes
    __pycache__/.pytest_cache byproducts of the agent running pytest inside
    the copy — bytecode-compiled binaries that git would otherwise stage as
    "new files", cluttering diff.patch with noise unrelated to the agent's
    actual edits (harmless to the text-based oracles, which only look at
    '+' lines, but worth keeping out of the receipt).
    """
    subprocess.run(
        ["git", "add", "-A", "--", ".", *[f":!{a}" for a in HARNESS_ARTIFACTS]],
        cwd=run_dir,
        env=GIT_ENV,
    )
    proc = subprocess.run(
        ["git", "diff", "--cached", "--no-color"],
        cwd=run_dir,
        capture_output=True,
        text=True,
        env=GIT_ENV,
    )
    return proc.stdout


def append_receipt(row: dict) -> None:
    RESULTS_PATH.parent.mkdir(parents=True, exist_ok=True)
    rows = []
    if RESULTS_PATH.exists():
        rows = load_json(RESULTS_PATH)
    rows.append(row)
    RESULTS_PATH.write_text(json.dumps(rows, indent=2, default=str))


def do_run(arm: str, task: str, seed: int, skip_claude: bool = False) -> dict:
    notes = load_json(NOTES_PATH)
    tasks = load_json(TASKS_PATH)
    task_prompt = tasks[task]

    run_dir = run_dir_for(arm, task, seed)
    prepare_copy(run_dir)

    session = requests.Session()
    session.trust_env = False

    arm_meta = {}
    project_id = None
    try:
        if arm == "none":
            arm_meta = setup_arm_none(run_dir)
        elif arm == "curated":
            arm_meta = setup_arm_curated(run_dir, notes)
        elif arm == "engram":
            arm_meta = setup_arm_engram(run_dir, notes, session)
            project_id = arm_meta["project_id"]
        else:
            raise ValueError(arm)

        # Commit memory scaffolding (CLAUDE.md / .mcp.json / skills) as part of
        # the "pristine" baseline so the agent's own git diff only shows its
        # actual work, not the harness setup.
        subprocess.run(["git", "add", "-A"], cwd=run_dir, env=GIT_ENV)
        subprocess.run(
            ["git", "commit", "-q", "--allow-empty", "-m", "arm setup"],
            cwd=run_dir,
            env=GIT_ENV,
        )

        notes_for_report = {"arm_meta_summary": {k: v for k, v in arm_meta.items() if k != "node_ids"}}

        claude_result = {}
        if not skip_claude:
            claude_result = run_claude(arm, task_prompt, run_dir)

        diff_text = capture_diff(run_dir)
        (run_dir / "diff.patch").write_text(diff_text)

        # Oracles run even under --skip-claude: it's a dry run of the harness
        # plumbing (setup + hidden-test wiring + pytest invocation), not a
        # claim that they'd pass — with no agent changes most legitimately
        # fail (e.g. T1's push_batch doesn't exist yet).
        oracle_results = oracles.run_oracles_for_task(str(VENV_PYTHON), run_dir, task, diff_text)

        result_event = claude_result.get("result_event") or {}
        cost_usd = result_event.get("total_cost_usd")
        num_turns = result_event.get("num_turns")
        duration_ms = result_event.get("duration_ms")

        row = {
            "phase": RAKE_PHASE,
            "arm": arm,
            "task": task,
            "seed": seed,
            "model": "sonnet",
            "run_dir": str(run_dir),
            "skip_claude": skip_claude,
            "oracles": {k: v for k, v in oracle_results.items() if not k.startswith("_")},
            "oracles_detail": {k: v for k, v in oracle_results.items() if k.startswith("_")},
            "engram_calls": claude_result.get("engram_calls", {}),
            "tool_calls": claude_result.get("tool_calls", {}),
            "cost_usd": cost_usd,
            "num_turns": num_turns,
            "duration_ms": duration_ms,
            "returncode": claude_result.get("returncode"),
            "hit_max_turns_or_error": claude_result.get("hit_max_turns_or_error"),
            "permission_mode_fallback_used": claude_result.get("permission_mode_fallback_used"),
            "final_message_mentions_memory": _mentions_memory(claude_result.get("final_message_text", "")),
            "final_message_text": claude_result.get("final_message_text", "")[:2000],
            "notes": notes_for_report,
        }
        append_receipt(row)
        return row
    finally:
        if project_id:
            teardown_arm_engram(project_id, session)


def _mentions_memory(text: str) -> bool:
    lowered = (text or "").lower()
    keywords = ["engram", "memory", "retry budget", "with_retry", "decision", "caution", "tombstone"]
    return any(k in lowered for k in keywords)


def print_table() -> None:
    if not RESULTS_PATH.exists():
        print("no results yet")
        return
    rows = load_json(RESULTS_PATH)
    print(f"{'arm':10} {'task':4} {'seed':4} {'oracles':60} {'cost':>8} {'turns':>6}")
    for r in rows:
        oc = r.get("oracles", {})
        oc_str = " ".join(f"{k}={'P' if v else 'F'}" for k, v in oc.items())
        cost = r.get("cost_usd")
        cost_str = f"{cost:.4f}" if isinstance(cost, (int, float)) else "?"
        print(f"{r['arm']:10} {r['task']:4} {r['seed']:<4} {oc_str:60} {cost_str:>8} {str(r.get('num_turns')):>6}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm", choices=["none", "curated", "engram"], required=False)
    ap.add_argument("--task", choices=["T1", "T2", "T3"], required=False)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--skip-claude", action="store_true", help="dry run: setup + oracles only")
    ap.add_argument("--print-table", action="store_true")
    args = ap.parse_args()

    if args.print_table:
        print_table()
        return

    if not args.arm or not args.task:
        ap.error("--arm and --task are required unless --print-table")

    row = do_run(args.arm, args.task, args.seed, skip_claude=args.skip_claude)
    print(json.dumps({k: v for k, v in row.items() if k not in ("final_message_text", "notes")}, indent=2, default=str))


if __name__ == "__main__":
    main()
