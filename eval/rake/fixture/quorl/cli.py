"""quorl command line: parse and push frame files."""

import argparse
import sys

from quorl.config import get, load_config
from quorl.frames import parse_frames
from quorl.relay import Uplink, UplinkError
from quorl.vault import Vault


def cmd_parse(args):
    frames = parse_frames(open(args.file, "rb").read())
    print(f"{len(frames)} frames")
    for frame in frames:
        print(f"  #{frame.seq}: {len(frame.payload)} bytes")
    return 0


def cmd_push(args):
    cfg = load_config()
    uplink = Uplink(get(cfg, "relay.endpoint"), get(cfg, "relay.fail_rate"))
    vault = Vault(get(cfg, "vault.path"))
    dropped = 0
    # Record every ack in the same transaction we sent it in, then flush
    # before leaving the block (see Vault.transaction()'s docstring).
    with vault.transaction():
        for frame in parse_frames(open(args.file, "rb").read()):
            try:
                ack = uplink.send(frame)
                vault.record(frame.seq, ack)
                print(ack)
            except UplinkError as exc:
                dropped += 1
                print(f"dropped: {exc}", file=sys.stderr)
        vault.sync_ledger()
    return 1 if dropped else 0


def main(argv=None):
    parser = argparse.ArgumentParser(prog="quorl")
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("parse", help="list the frames in a file")
    p.add_argument("file")
    p.set_defaults(func=cmd_parse)
    p = sub.add_parser("push", help="push every frame over the uplink")
    p.add_argument("file")
    p.set_defaults(func=cmd_push)
    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
