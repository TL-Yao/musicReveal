# 交叉核对

用参考 Python 实现（music-geshizhuanhuan，MIT）的 `tests/builders.py` 构造合成加密样本，再分别交给参考解码器与 reveal-core（`examples/reveal`）解密，逐字节比较两者输出及原始明文。覆盖 NCM、QMC v1、QMC v2 Map、QMC v2 RC4（QTag / PcV1Legacy 两种文件尾、ekey v1 / EncV2 两种编码），每类都有 1.5MB 样本（跨 RC4 0x1400 分段与 0x7FFF 回绕边界），另有 40MB 样本兼做计时。样本只写临时目录，跑完即删。

在仓库根运行：

```sh
~/.local/bin/uv run --python 3.13 --with pycryptodome --with mutagen tools/crosscheck/crosscheck.py
```

- 参考实现路径默认是本机的任务 refs 目录，可用 `REVEAL_REF=<music-geshizhuanhuan 目录>` 覆盖。
- 大样本大小默认 40MB，可用 `REVEAL_BIG_MB=<n>` 调整。
- 脚本会先执行 `cargo build --release -p reveal-core --example reveal`。

参考实现的已知局限（因此脚本选了特定的主密钥长度）：它的 Map 用「128 字节压缩表 + offset % 128」，只有主密钥长度整除 256 时才与标准 Map 公式 `index = (o² + 71214) % n` 一致；它的 RC4 段跳过量用 `& 0x1FF`，只有主密钥长度为 512 时才与标准的 `% n` 一致。reveal-core 按标准公式实现（与 qmunlock 一致），所以样本用 256/128 字节的 Map 主密钥和 512 字节的 RC4 主密钥。
