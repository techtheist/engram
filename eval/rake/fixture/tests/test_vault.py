from quorl.vault import Vault


def test_transaction_logs_enter_and_exit():
    vault = Vault()
    with vault.transaction() as v:
        v.record(0, "ack:0")
    assert vault.calls == ["txn_enter", "record", "txn_exit"]
    assert vault.entries == [(0, "ack:0")]


def test_sync_returns_entry_count():
    vault = Vault()
    with vault.transaction():
        vault.record(0, "ack:0")
        vault.record(1, "ack:1")
    assert vault.sync_ledger() == 2
    assert vault.calls[-1] == "sync"
