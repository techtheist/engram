from pytest import raises

from quorl.frames import Frame
from quorl.relay import Uplink, UplinkError


def _run(seed):
    results = []
    uplink = Uplink(fail_rate=0.5, seed=seed)
    for seq in range(8):
        try:
            results.append(uplink.send(Frame(seq, b"x")))
        except UplinkError:
            results.append("drop")
    assert uplink.attempts == 8
    return results


def test_seeded_uplink_drops_deterministically():
    results = _run(7)
    assert results == _run(7)
    assert "drop" in results and any(r.startswith("ack:") for r in results)


def test_zero_fail_rate_never_drops():
    uplink = Uplink(fail_rate=0.0)
    assert uplink.send(Frame(3, b"x")) == "ack:3"
    with raises(UplinkError):
        Uplink(fail_rate=1.0).send(Frame(0, b"x"))
