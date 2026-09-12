"""The uplink: delivers frames to the ledger relay, dropping some of them."""

import random


class UplinkError(RuntimeError):
    """The uplink dropped the frame."""


class Uplink:
    def __init__(self, endpoint="ledger://local", fail_rate=0.3, seed=0):
        self.endpoint = endpoint
        self.fail_rate = fail_rate
        self._rng = random.Random(seed)
        self.attempts = 0
        self.sent = []

    def send(self, frame):
        """Deliver one frame; returns an ack string or raises UplinkError."""
        self.attempts += 1
        if self._rng.random() < self.fail_rate:
            raise UplinkError(f"uplink dropped frame {frame.seq}")
        self.sent.append(frame.seq)
        return f"ack:{frame.seq}"
