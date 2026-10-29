# AGENTS.md

- 发版前整套测试要对着随包的精简 ffmpeg 再跑一遍：`scripts/package.sh` 后 `REVEAL_FFMPEG=$PWD/dist/musicReveal.app/Contents/MacOS/ffmpeg REVEAL_GEN_FFMPEG=/opt/homebrew/bin/ffmpeg cargo test --workspace -- --include-ignored`（开发机的完整 ffmpeg 会遮住随包版缺的 muxer/解码器；生成素材走 REVEAL_GEN_FFMPEG）。
