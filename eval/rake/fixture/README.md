# quorl — ledger relay

Quorl turns a byte stream of *frames* into acknowledged entries in a local
*vault* ledger, relayed over an *uplink*.

## Layout

- `quorl/frames.py` — `parse_frames(data)` splits a byte string into `Frame`
  objects; each frame carries a payload and a trailing checksum.
- `quorl/relay.py` — `Uplink.send(frame)` delivers one frame and returns an
  acknowledgement, or raises `UplinkError` when the link drops it.
- `quorl/vault.py` — `Vault` keeps the ledger: `record()` entries inside a
  `transaction()`, `sync_ledger()` flushes them.
- `quorl/retry.py` — `with_retry(fn, ...)` runs a callable with exponential
  backoff.
- `quorl/config.py` — `load_config()` reads settings.
- `quorl/cli.py` — the `quorl` command line.

## Usage

```
python3 -m quorl.cli parse frames.bin      # count and list frames
python3 -m quorl.cli push frames.bin       # push every frame over the uplink
```

## Configuration

Quorl reads settings from `~/.quorl/config.toml` in your home directory,
so one file covers every checkout on the machine:

```toml
[relay]
endpoint = "ledger://local"
fail_rate = 0.3

[vault]
path = ".quorl-vault"
```

A repo-local `quorl.toml` at the repository root, if present, overrides
any of these settings for that one checkout.

## Tests

```
python3 -m pytest
```

## Changelog

- 0.3: removed `--skip-checksum` / `verify=False` (see decision log).
