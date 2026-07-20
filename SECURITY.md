# Security

Engram Alpha is a **local-first, single-user** tool: one daemon per machine,
bound to loopback, holding graph files that live inside your repos. There are
no accounts, no cloud, and no telemetry — the threat model is your own
machine, your browser, and the AI assistants you connect. The graph itself is
sensitive by nature (it is a durable record of your project's reasoning), so
it is treated as data worth protecting, not as a cache.

## Threat model & measures

**Network surface.** The daemon binds `127.0.0.1` only — nothing listens on
external interfaces. The MCP endpoint (`/mcp`) validates the `Host` header
against loopback (rmcp's default), which blocks DNS-rebinding attacks against
it. The `engram-alpha mcp` bridge and the brief hook talk only to a daemon
they have verified over `/health` as serving *their* repo's store.

**Secrets in memory.** Every write — titles, bodies, imports included — runs
a server-side redaction pass (`engram-core/src/redact.rs`) that scrubs
credential-shaped content (cloud keys, tokens, private-key blocks), and the
capture skill instructs assistants never to store secrets in the first place.
Redaction is defense in depth, not a guarantee: review the pane, and treat
the graph file like you treat your shell history.

**Data at rest.** Graph stores (`.engram/graph.db` / `graph.tepin`) stay
inside the repo, are git-ignored by `setup` (and checked by `doctor`), and
never leave the machine unless you explicitly export. They are protected by
OS file permissions only — see *Known gaps* for the encryption plan.

**Memory poisoning.** An assistant can be prompt-injected by hostile content
into writing false "knowledge". The trust model limits the blast radius:
assistant writes start provisional and decay unless deliberately confirmed;
look-alike conflicts are queued for human judgment; every mutation lands in
an append-only audit journal with per-session attribution; supersession
archives rather than deletes; hard delete is a user-only gesture. The pane is
the review surface — writes are silent, but never invisible.

**Supply chain.** Dependencies are locked (`Cargo.lock` committed; the
TepinDB driver is pinned by git revision). Binary self-update downloads only
from this repository's GitHub Releases and verifies the artifact against the
release's published SHA-256 before swapping itself. Cortex models download
over HTTPS from their recorded Hugging Face URLs into `~/.cache/engram/`.

## Known gaps (tracked, in the open)

- **No encryption at rest — yet.** App-level encryption of the graph stores
  is planned for a later release; this release deliberately ships without it.
  Until then, disk encryption (FileVault/LUKS/BitLocker) is the effective
  at-rest protection.
- **Permissive CORS on the localhost API.** Any page in your browser can
  currently call the daemon's REST API. Hardening to an origin allowlist
  (localhost + the IDE webview origins the pane embeds under) is scheduled
  before release; `/mcp` is already loopback-`Host`-validated.
- **No local authentication.** Any process on your machine can use the API —
  consistent with the single-user local trust model, but stated plainly.
- **Model files are not checksum-pinned.** Unlike the binary self-update,
  cortex model downloads verify HTTPS transport but not a pinned digest;
  adding per-file SHA-256 to the model specs is planned.

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting on this repository.
We'll acknowledge within a few days.
