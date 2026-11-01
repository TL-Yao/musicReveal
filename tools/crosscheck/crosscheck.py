"""交叉核对：同一批合成输入，参考 Python 实现与 reveal-core 的解密输出逐字节比较。

运行方式见同目录 README.md。样本只在临时目录生成，跑完即删。
"""
from __future__ import annotations

import os
import random
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
REF = Path(os.environ.get("REVEAL_REF") or sys.exit("请用 REVEAL_REF 指定参考实现（music-geshizhuanhuan）的目录"))
sys.path[:0] = [str(REF), str(REF / "tests")]

from builders import build_ncm, build_qmc_v1, build_qmc_v2, make_ekey_v1, make_ekey_v2  # noqa: E402
from music_unlock.ciphers import make_qmc2_stream  # noqa: E402
from music_unlock.formats.base import DecodeOptions  # noqa: E402
from music_unlock.formats.ncm import NcmDecoder  # noqa: E402
from music_unlock.formats.qmc import QmcDecoder  # noqa: E402

MB = 1 << 20
BIG = int(os.environ.get("REVEAL_BIG_MB", "40")) * MB


def plain(n: int, seed: int) -> bytes:
    return b"ID3\x03\x00\x00\x00\x00\x00\x00" + random.Random(seed).randbytes(n - 10)


def pc_legacy(data: bytes, master: bytes, ekey: str) -> bytes:
    enc = make_qmc2_stream(master).decrypt(data)
    return enc + ekey.encode() + struct.pack("<I", len(ekey))


# 说明：参考实现的 Map 用「128 字节压缩表 + offset % 128」，只有主密钥长度整除 256 时
# 才与标准 Map（index = (o² + 71214) % n）一致；RC4 的段跳过量用 `& 0x1FF`，只有 n = 512 时
# 与标准 `% n` 一致。故这里 Map 用 256/128 字节、RC4 用 512 字节主密钥。
MAP256 = bytes((i * 7 + 1) & 0xFF for i in range(256))
MAP128 = bytes((i * 11 + 3) & 0xFF for i in range(128))
RC4_512 = bytes((i * 7 + 3) & 0xFF for i in range(512))
META = {
    "musicId": "1352449924",
    "musicName": "交叉核对",
    "artist": [["甲", "1"], ["乙", 0]],
    "album": "专辑",
    "format": "mp3",
}

CASES = [
    # (名称, 文件名, 明文, 构造函数, 参考解码器)
    ("NCM 1.5MB", "a.ncm", plain(3 * MB // 2, 1),
     lambda p: build_ncm(p, b"0123456789abcdef", META, b"\x89PNGcover"), NcmDecoder),
    ("QMC v1 1.5MB", "b.bkcmp3", plain(3 * MB // 2, 2), build_qmc_v1, QmcDecoder),
    ("QMC v2 Map/ekey v1/QTag 1.5MB", "c.mflac", plain(3 * MB // 2, 3),
     lambda p: build_qmc_v2(p, MAP256, make_ekey_v1(MAP256)), QmcDecoder),
    ("QMC v2 Map/EncV2/PcV1Legacy 1.5MB", "d.mgg", plain(3 * MB // 2, 4),
     lambda p: pc_legacy(p, MAP128, make_ekey_v2(MAP128)), QmcDecoder),
    ("QMC v2 RC4/EncV2/QTag 1.5MB", "e.mgg", plain(3 * MB // 2, 5),
     lambda p: build_qmc_v2(p, RC4_512, make_ekey_v2(RC4_512)), QmcDecoder),
    ("QMC v2 RC4/ekey v1/PcV1Legacy 1.5MB", "f.mflac", plain(3 * MB // 2, 6),
     lambda p: pc_legacy(p, RC4_512, make_ekey_v1(RC4_512)), QmcDecoder),
    (f"NCM {BIG // MB}MB", "big.ncm", plain(BIG, 7),
     lambda p: build_ncm(p, b"bigkey-0123456789", META), NcmDecoder),
    (f"QMC v2 Map {BIG // MB}MB", "big.mflac", plain(BIG, 9),
     lambda p: build_qmc_v2(p, MAP256, make_ekey_v1(MAP256)), QmcDecoder),
    (f"QMC v2 RC4 {BIG // MB}MB", "big.mgg", plain(BIG, 8),
     lambda p: build_qmc_v2(p, RC4_512, make_ekey_v1(RC4_512)), QmcDecoder),
]


def main() -> int:
    subprocess.run(
        ["cargo", "build", "--release", "-q", "-p", "reveal-core", "--example", "decrypt_only"],
        cwd=ROOT, check=True,
    )
    exe = ROOT / "target" / "release" / "examples" / "decrypt_only"
    failed = 0
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        for name, fname, data, build, decoder in CASES:
            src = tmp / fname
            src.write_bytes(build(data))
            ref = decoder().decode(src, DecodeOptions()).payload
            out = tmp / (fname + ".out")
            t = time.perf_counter()
            proc = subprocess.run([exe, src, out], capture_output=True, text=True)
            wall = time.perf_counter() - t
            got = out.read_bytes() if proc.returncode == 0 else b""
            ok = got == ref and ref == data
            failed += not ok
            print(f"[{'PASS' if ok else 'FAIL'}] {name}: 参考 {len(ref)}B / reveal {len(got)}B"
                  f" / 明文 {len(data)}B；reveal: {proc.stdout.strip() or proc.stderr.strip()}"
                  f"（进程墙钟 {wall:.3f}s）")
            if not ok and got:
                diff = next((i for i, (a, b) in enumerate(zip(got, ref)) if a != b), None)
                print(f"    首个差异 @{diff}")
            src.unlink()
            out.unlink(missing_ok=True)
    print(f"\n交叉核对：{len(CASES) - failed}/{len(CASES)} 通过")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
