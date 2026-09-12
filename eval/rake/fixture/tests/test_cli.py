from quorl.cli import main
from quorl.frames import encode_frame


def test_parse_subcommand_lists_frames(tmp_path, capsys):
    path = tmp_path / "frames.bin"
    path.write_bytes(encode_frame(b"one") + encode_frame(b"three"))
    assert main(["parse", str(path)]) == 0
    out = capsys.readouterr().out
    assert out.startswith("2 frames")
    assert "#1: 5 bytes" in out
