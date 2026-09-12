"""Configuration loading for quorl."""

import os
import tomllib
from pathlib import Path

CONFIG_NAME = "quorl.toml"

DEFAULTS = {
    "relay": {"endpoint": "ledger://local", "fail_rate": 0.3},
    "vault": {"path": ".quorl-vault"},
}


def _merge(base, override):
    out = {k: (dict(v) if isinstance(v, dict) else v) for k, v in base.items()}
    for key, value in override.items():
        if isinstance(value, dict) and isinstance(out.get(key), dict):
            out[key] = _merge(out[key], value)
        else:
            out[key] = value
    return out


def repo_config_path(root=None):
    """The repo-local config file: <root>/quorl.toml."""
    return Path(root or os.getcwd()) / CONFIG_NAME


def _home_config_path():
    """The home-directory config file: ~/.quorl/config.toml."""
    home = Path(os.environ.get("QUORL_HOME") or Path.home())
    return home / ".quorl" / "config.toml"


def load_config(root=None):
    """Load settings, merged over DEFAULTS.

    Reads ~/.quorl/config.toml first, so one file can cover every checkout
    on the machine, then layers <root>/quorl.toml on top so a single
    checkout can override any of those settings.
    """
    merged = dict(DEFAULTS)
    home_path = _home_config_path()
    if home_path.exists():
        merged = _merge(merged, tomllib.loads(home_path.read_text()))
    repo_path = repo_config_path(root)
    if repo_path.exists():
        merged = _merge(merged, tomllib.loads(repo_path.read_text()))
    return merged


def get(config, dotted, default=None):
    """Fetch a nested key: get(cfg, "relay.endpoint")."""
    node = config
    for part in dotted.split("."):
        if not isinstance(node, dict) or part not in node:
            return default
        node = node[part]
    return node
