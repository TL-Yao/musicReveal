# 第三方许可声明

musicReveal 本身以 MIT 许可发布，见 [LICENSE](LICENSE)。它使用或参考了以下第三方作品。

## 1. 参考项目（MIT）

`crates/reveal-core/src/qmc.rs`、`crates/reveal-core/src/ncm.rs` 中的算法常量（QMC v1 静态密钥表、NCM AES 密钥、Tencent TEA 常量、EncV2 密钥、Map/RC4 规则）参考或移植自以下两个以 MIT 许可发布的项目：

- [mary20050520/qmunlock](https://github.com/mary20050520/qmunlock)
- [HRuiCcc/music-geshizhuanhuan](https://github.com/HRuiCcc/music-geshizhuanhuan)

两者的许可全文附在本文末尾，也保存在 `crates/reveal-core/THIRD_PARTY_LICENSES.md`。

## 2. FFmpeg 与 LAME（LGPL）

打包后的 `musicReveal.app` 在 `Contents/MacOS/ffmpeg` 中附带一个独立的 ffmpeg 可执行文件，用于转码为 MP3 / FLAC。它由以下源码编译：

- [FFmpeg](https://ffmpeg.org/) 9.0.2，GNU Lesser General Public License 2.1 或更高版本；
- [LAME](https://lame.sourceforge.io/) 3.100，GNU Lesser General Public License 2 或更高版本，静态链接进上述 ffmpeg。

编译时未启用任何 GPL 或 nonfree 组件，`configure` 报告的许可为 LGPL version 2.1 or later。对应源码（FFmpeg、LAME 源码包原样与构建脚本）以 `musicReveal_<版本>_ffmpeg-lame-source.tar` 随 dmg 一同发布；源码下载地址、SHA-256 校验值与全部 configure 参数见 `scripts/build-ffmpeg.sh`；应用包内的 `Contents/Resources/licenses/` 目录附有 FFmpeg 与 LAME 的许可全文和来源说明（`SOURCE.md`）。musicReveal 以独立进程调用 ffmpeg，不与之链接；用户可以用同样的源码和脚本自行编译，替换包内的 ffmpeg。

本仓库不包含 FFmpeg 或 LAME 的源码与二进制，它们只在运行 `scripts/package.sh` 时下载并编译。

## 3. Rust 依赖

Rust 依赖（Tauri、tokio、lofty、aes、reqwest，以及播放器用的 rodio、cpal、souvlaki、objc2-foundation 等）的许可以 MIT、Apache-2.0 及其组合为主，另有少量 BSD-3-Clause、ISC、Zlib、Unicode-3.0、MPL-2.0 等宽松或弱著作权许可。

其中以 MPL-2.0 发布的组件（rodio 用于解码音频的 symphonia 系列，以及 Tauri 间接依赖的 cssparser、selectors、dtoa-short、option-ext 等）均未经修改，原样编译进应用。按 MPL-2.0 的要求，生成的清单在每个 MPL-2.0 组件下列出了对应版本源码的取得方式：crates.io 上该版本的下载地址（`https://crates.io/api/v1/crates/<名称>/<版本>/download`）与源码仓库地址。完整清单与许可全文由 [cargo-about](https://github.com/EmbarkStudios/cargo-about) 根据 `Cargo.lock` 生成：

```sh
cargo install --locked cargo-about
cargo about generate --locked -c scripts/about.toml scripts/about.hbs -o rust-dependencies.md
```

`scripts/package.sh` 会自动生成这份清单，并放进应用包的 `Contents/Resources/licenses/rust-dependencies.md`。

## 4. 前端依赖

打进前端产物的 npm 包只有 `@tauri-apps/api` 与 `@tauri-apps/plugin-dialog`，均为 `Apache-2.0 OR MIT`。构建工具（Vite、TypeScript、Tauri CLI）不随应用分发。查看全部 npm 依赖的许可可以在 `app/` 目录运行：

```sh
npx license-checker --summary
```

---

## 附：参考项目许可全文

### music-geshizhuanhuan

```
MIT License

Copyright (c) 2026 music-geshizhuanhuan contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### qmunlock

```
MIT License

Copyright (c) 2026 QM Unlock contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
