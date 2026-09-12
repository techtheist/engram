from pytest import raises

from quorl import retry
from quorl.retry import RetryExhausted, with_retry


def test_default_attempts_is_three(monkeypatch):
    monkeypatch.setattr(retry.time, "sleep", lambda _: None)
    calls = {"n": 0}

    def always():
        calls["n"] += 1
        raise RuntimeError("nope")

    with raises(RetryExhausted):
        with_retry(always)
    assert calls["n"] == 3


def test_succeeds_after_failures(monkeypatch):
    sleeps = []
    monkeypatch.setattr(retry.time, "sleep", sleeps.append)
    calls = {"n": 0}

    def flaky():
        calls["n"] += 1
        if calls["n"] < 3:
            raise RuntimeError("nope")
        return "ok"

    assert with_retry(flaky, base_delay=0.1) == "ok"
    assert calls["n"] == 3
    assert sleeps == [0.1, 0.2]


def test_gives_up(monkeypatch):
    monkeypatch.setattr(retry.time, "sleep", lambda _: None)

    def always():
        raise RuntimeError("nope")

    with raises(RetryExhausted):
        with_retry(always, attempts=3)
