"""Frame wire format: [2-byte big-endian length][payload][1-byte checksum]."""

from dataclasses import dataclass


class FrameError(ValueError):
    """Malformed or truncated frame."""


class ChecksumError(FrameError):
    """A frame's checksum did not match its payload."""


@dataclass
class Frame:
    seq: int
    payload: bytes


def checksum(payload):
    """Additive checksum: the byte sum of the payload, modulo 256."""
    total = 0
    for byte in payload:
        total = (total + byte) % 256
    return total


def encode_frame(payload):
    if len(payload) > 0xFFFF:
        raise FrameError("payload too long")
    return len(payload).to_bytes(2, "big") + payload + bytes([checksum(payload)])


def parse_frames(data):
    """Split data into frames, verifying every checksum."""
    frames = []
    seq = 0
    while data:
        if len(data) < 3:
            raise FrameError(f"truncated header at frame {seq}")
        length = int.from_bytes(data[:2], "big")
        end = 2 + length
        if len(data) < end + 1:
            raise FrameError(f"truncated payload at frame {seq}")
        payload = data[2:end]
        if checksum(payload) != data[end]:
            raise ChecksumError(f"checksum mismatch at frame {seq}")
        frames.append(Frame(seq, payload))
        data = data[end + 1 :]
        seq += 1
    return frames
