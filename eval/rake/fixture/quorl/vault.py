"""The vault: an in-memory ledger with transactions and a sync step."""

from contextlib import contextmanager


class Vault:
    def __init__(self, path=".quorl-vault"):
        self.path = path
        self.entries = []
        self.calls = []
        self.synced_inside_txn = False
        self._depth = 0

    @contextmanager
    def transaction(self):
        """Open a ledger transaction.

        Call sync_ledger() before leaving the block so the ledger is
        flushed while the transaction is still open.
        """
        self.calls.append("txn_enter")
        self._depth += 1
        try:
            yield self
        finally:
            self._depth -= 1
            self.calls.append("txn_exit")

    def record(self, seq, ack):
        self.calls.append("record")
        self.entries.append((seq, ack))

    def sync_ledger(self):
        """Flush the ledger; returns the number of entries flushed."""
        self.calls.append("sync")
        if self._depth > 0:
            self.synced_inside_txn = True
        return len(self.entries)
