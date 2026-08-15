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

**Two layers, two protections.** Engram stores your knowledge in two places,
and they are protected differently on purpose:

| | curated graph (`graph.tepin`) | session history (`history.tepin`) |
|---|---|---|
| **Redacted on write** | yes — its *only* content protection | yes, before sealing |
| **Encrypted at rest** | **no** | **yes** (XChaCha20-Poly1305) |
| **You can read it** | yes — that is the point | only through the pane/tools |

The curated graph is **redacted but not encrypted**, because it exists to be
inspected: you read it in the pane, you edit it, you delete from it, and
`npx tepindb` can open it. Encrypting it would buy at-rest protection at the
cost of the inspectability the whole product is built on. Its content
protection is therefore redaction plus OS file permissions plus your disk
encryption — and, more than either, the fact that a human curates it.

The history layer is **redacted *and* encrypted**, because nobody curates a
transcript. It records raw conversation you never reviewed, in bulk, so it
cannot rely on your judgment the way the graph does and gets a cipher instead
(details below). Recording is opt-in for exactly this reason.

**Secrets in memory.** Every write — titles, bodies, imports, and history
plaintext before it is sealed — runs a server-side redaction pass
(`engram-core/src/redact.rs`). It has two layers:

1. **Named patterns**, which are what actually catch secrets: PEM private-key
   blocks, AWS access key ids, JWTs, GitHub / Slack / OpenAI-style tokens,
   credentials embedded in URLs, and `key = value` assignments for
   password/token/secret-shaped keys (the value is masked, the key kept so the
   note still reads).
2. **A high-entropy backstop** for opaque tokens with no recognisable shape.

The backstop deliberately does **not** try to be clever. Since 0.8.7 it judges
entropy per separator-delimited *segment* rather than over a whole token,
because the previous whole-token rule masked compound technical identifiers —
model slugs like `cross-encoder/nli-deberta-v3-small`, target triples like
`x86_64-unknown-linux-gnu`, long URLs — and those losses were **permanent and
silent**: the original is never stored anywhere, so a memory system quietly
lost the name of its own model. Real credential material has no
dictionary-shaped parts, so segment-level entropy separates the two cleanly. A
token that carries no separators is judged exactly as before.

That relaxation is affordable precisely because of the split above: the
curated graph is user-visible and user-curated by design, so an
over-aggressive backstop costs real knowledge while buying little, and history
— where content arrives unreviewed and in volume — has encryption underneath
it rather than resting on redaction alone.

Redaction is defense in depth, not a guarantee. The capture skill instructs
assistants never to store secrets in the first place; review the pane, and
treat the graph file like you treat your shell history.

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
