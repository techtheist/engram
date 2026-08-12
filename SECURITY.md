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
never leave the machine unless you explicitly export. The curated graph is
protected by OS file permissions (plus full-disk encryption if you run it —
recommended).

**Session history at rest (0.8.4).** The history layer
(`.engram/history.tepin`) records raw assistant conversations, so it gets
more: message and session **text is sealed** — zstd-compressed, then
encrypted with XChaCha20-Poly1305 under a per-machine 256-bit key minted on
first need and stored in the OS keystore (macOS Keychain / Windows credential
store / Linux secret-service), with a `~/.engram/history.key` (0600) fallback
for headless machines (`ENGRAM_KEYRING=off` forces the file). Honest scope:

- **Protects**: copied `.tepin` files, backups, stolen disks without FDE,
  other OS users. Losing the key makes history unreadable; the curated graph
  is unaffected.
- **Does NOT protect** against malware running as your user — it can read the
  keystore exactly like the daemon does. That boundary belongs to the OS.
- **Stays plaintext, by design**: node/edge structure, types, timestamps,
  session ids — and **embedding vectors**. Vectors admit inversion attacks
  that recover the *gist* of a message, not its text; sealing them would
  break vector-first retrieval. Stated here so nobody mistakes the layer for
  more than it is.
- **No keyword index over history text**: history search is vector-first, and
  candidate text is decrypted in memory at query time only. (The index would
  otherwise persist exactly the plaintext the seal protects.)
- Redaction runs on the plaintext **before** sealing — secrets never reach
  the store, encrypted or not.

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

- **The curated graph is not encrypted at rest.** 0.8.4 sealed the history
  layer (see above) — the deliberately-scoped first step of the app-level
  encryption thread. The curated store still relies on OS permissions and
  disk encryption (FileVault/LUKS/BitLocker); extending sealing to it is a
  separate decision (it would cost the pane's `npx tepindb` inspectability).
- **Deleted history can linger in freed pages.** Hard-deleting a session (or
  wiping the layer) removes the rows, but the storage engine's freed pages
  aren't scrubbed — the same caveat already documented for curated hard
  deletes. Sealed rows reduce this to ciphertext residue.
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
