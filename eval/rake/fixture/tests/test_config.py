from quorl.config import DEFAULTS, get, load_config


def _write_home_config(tmp_path, text):
    home = tmp_path / "home"
    (home / ".quorl").mkdir(parents=True)
    (home / ".quorl" / "config.toml").write_text(text)
    return home


def test_repo_local_config_overrides_home(tmp_path, monkeypatch):
    home = _write_home_config(
        tmp_path, '[relay]\nendpoint = "ledger://home"\nfail_rate = 0.9\n'
    )
    monkeypatch.setenv("QUORL_HOME", str(home))
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / "quorl.toml").write_text('[relay]\nendpoint = "ledger://test"\n')

    cfg = load_config(repo)
    assert get(cfg, "relay.endpoint") == "ledger://test"
    # repo-local didn't mention fail_rate, so the home value still applies
    assert get(cfg, "relay.fail_rate") == 0.9


def test_home_config_used_when_no_repo_config(tmp_path, monkeypatch):
    home = _write_home_config(tmp_path, '[relay]\nendpoint = "ledger://home"\n')
    monkeypatch.setenv("QUORL_HOME", str(home))

    cfg = load_config(tmp_path / "repo-with-no-quorl-toml")
    assert get(cfg, "relay.endpoint") == "ledger://home"
    assert get(cfg, "relay.fail_rate") == DEFAULTS["relay"]["fail_rate"]


def test_missing_config_gives_defaults(tmp_path, monkeypatch):
    monkeypatch.setenv("QUORL_HOME", str(tmp_path / "nohome"))
    assert load_config(tmp_path) == DEFAULTS
