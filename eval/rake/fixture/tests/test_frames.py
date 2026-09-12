from pytest import raises

from quorl.frames import ChecksumError, FrameError, encode_frame, parse_frames


def test_roundtrip():
    data = encode_frame(b"alpha") + encode_frame(b"") + encode_frame(b"beta")
    frames = parse_frames(data)
    assert [f.payload for f in frames] == [b"alpha", b"", b"beta"]
    assert [f.seq for f in frames] == [0, 1, 2]


def test_corrupted_frame_is_rejected():
    data = bytearray(encode_frame(b"alpha") + encode_frame(b"beta"))
    data[2 + 5 + 1 + 2] ^= 0x01  # flip a bit in the second payload
    with raises(ChecksumError):
        parse_frames(bytes(data))


def test_truncated_frame_is_rejected():
    with raises(FrameError):
        parse_frames(encode_frame(b"alpha")[:-1])
