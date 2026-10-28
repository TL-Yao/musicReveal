# musicReveal

musicReveal 是一个 macOS 桌面工具：监听你指定的文件夹，把 QQ 音乐、网易云音乐下载的加密音乐在本机转换成普通音频文件，并补全封面、歌词等元数据。

<!-- 截图位：主窗口（转换列表）、首次打开引导、设置页。截图之后补充。 -->

## 功能

- **支持的格式**
  - QQ 音乐新版加密格式（`mflac`、`mgg` 等 musicex 格式），需要本机已安装并登录 QQ 音乐；
  - QQ 音乐旧格式（`qmcflac`、`qmcogg`、`qmc0`/`qmc2`/`qmc3`、`bkc*`… 以及文件内自带密钥的 `mflac`/`mgg`）。**旧格式没有用真实文件验证过**：只用合成样本做了测试，并与参考项目的 Python 实现对同一输入的输出逐字节交叉核对；
  - 网易云音乐 `ncm`，无需登录。
- **输出**：保持原格式（FLAC、OGG、M4A、MP3 等），或统一转为 MP3（由应用内置的 ffmpeg 完成，无需另行安装）：本来就是 MP3 的保留原码率、不重新编码，其余编码为 320 kbps。转 MP3 时保留文件自带的全部标签。
  - 例外：解密后是 APE 或 WMA 的（`bkcape`、`bkcwma` 等），因为这两种格式无法可靠写入完整标签，无论输出格式怎么设置都会转换——APE 在「原格式」下转为 FLAC（无损到无损）、在「MP3」下转为 MP3；WMA 一律转为 320 kbps MP3。
- **元数据补全**：保留文件自带的标签；再按文件里的歌曲 ID 向对应平台查询，补全年份、曲序、碟号、专辑艺人、高清封面和歌词（含翻译）。多位歌手全部保留。同时生成带时间轴的同名 `.lrc` 文件。
- **自动监听**：窗口打开期间，监听文件夹里新下载完成的歌曲会自动转换；窗口关闭期间下载的歌曲，下次打开时自动补转。也可以把文件拖进窗口手动转换。
- **不改动源文件**：转换结果写入输出目录（默认 `~/Music/musicReveal`），文件命名为「歌手 - 歌名」；同名文件自动加编号，不覆盖；已转换过的歌曲自动跳过（内容完全相同、只是换了文件名或位置的也会跳过）。

## 系统要求

在 Apple Silicon（arm64）Mac 上测试过。Intel Mac 与 Windows 未测试，不承诺支持。

## 安装与首次打开

1. 打开 `musicReveal_<版本>_aarch64.dmg`，把 musicReveal 拖到「应用程序」文件夹。
2. 首次打开时需要手动允许一次，原因见下文。

musicReveal 使用开发者自己生成的固定证书签名，没有经过 Apple 公证（notarization）。签名本身是完整有效的：`codesign --verify` 可以通过，应用内容被改动后签名会失效；但这个证书不来自 Apple，系统的 Gatekeeper 不认可它，`spctl --assess` 会显示 `rejected`，从网上下载的安装包第一次打开时会被拦下，提示无法验证开发者。处理方法：

- macOS 14 及更早版本：在「应用程序」文件夹里按住 Control 键点按（或右键点按）musicReveal，选择「打开」，在弹出的对话框中再点「打开」。
- macOS 15 及更新版本：先双击打开一次，看到拦截提示后点「完成」；然后进入「系统设置 → 隐私与安全性」，在页面下方找到关于 musicReveal 的提示，点「仍要打开」，按提示输入登录密码确认。

允许一次之后，之后的打开不再提示。因为证书固定不变，升级到用同一证书签名的新版本后，已经给过的授权（例如下文「需要授权」一节的权限）仍然有效。

自己从源码构建的应用没有「来自网络」的标记，直接打开即可。

## 使用

1. 首次打开时，添加要监听的文件夹。本机存在的 `~/Music/QQ音乐`、`~/Music/网易云音乐` 会预先勾选。
2. 确认输出目录和输出格式（原格式或 MP3）。这两项之后可以在设置中随时修改，只影响之后的转换。
3. 如果新加的文件夹里已有未转换的歌曲，应用会询问一次是否转换它们；否则只处理以后新下载的文件。
4. 之后保持窗口打开即可。在 QQ 音乐或网易云音乐下载歌曲，下载完成后会自动出现在列表中并完成转换。完成的歌曲可以在 Finder 中显示或直接播放。

关闭窗口即退出应用。正在转换的歌曲会中断并删除未完成的文件，下次打开时自动重新转换。

## 隐私

- 解密、转码、写标签全部在本机完成，音频文件不会上传到任何地方。
- 联网只用于补全元数据：按歌曲 ID 向 QQ 音乐、网易云音乐的公开接口查询歌曲信息、封面和歌词，不做模糊搜索，不发送音频内容。
- QQ 音乐新版格式的解密密钥需要向 QQ 音乐的接口获取，为此会读取本机 QQ 音乐客户端保存的登录信息。这些信息只在本机用于这一请求，不会保存到别处，也不会上传给第三方。界面上显示的账号是打码后的。
- 网易云音乐的文件不需要任何登录信息。

## 常见问题

**列表中的歌曲显示「待登录」**

这是 QQ 音乐新版格式的文件，解密需要 QQ 音乐的登录信息，而本机 QQ 音乐尚未登录或登录已过期。打开 QQ 音乐并登录后，这些歌曲会自动继续转换，不需要手动重试。

**QQ 音乐状态显示「需要授权」**

旧版 QQ 音乐把登录信息保存在受系统保护的位置，读取前需要你授权一次：点状态行中的「授权…」，系统设置会打开「隐私与安全性 → 完全磁盘访问权限」，在列表中打开 musicReveal 的开关，然后重新打开 musicReveal。将 QQ 音乐更新到最新版本后，登录信息存放在普通位置，不再需要这项授权。

**转换失败，原因是「文件不完整」**

文件仍在下载，或者下载中断导致文件被截断。等下载完成后点「重试」；如果文件本身已经损坏，请在 QQ 音乐或网易云音乐中重新下载。

**转为 MP3 时提示缺少 ffmpeg**

正式打包的应用内置了 ffmpeg。如果你运行的是自己用 `cargo` 或 `npx tauri dev` 启动的开发版本，需要自行安装 ffmpeg（例如 `brew install ffmpeg`），或用环境变量 `REVEAL_FFMPEG` 指定其路径。

## 从源码构建

需要：Xcode 命令行工具、Rust stable、Node.js 22，以及用于汇总依赖许可的 `cargo-about`（`cargo install --locked cargo-about`）。

```sh
# 以下命令都在仓库根目录执行

# 开发运行
(cd app && npm ci && npx tauri dev)

# 检查与测试
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
(cd app && npx tsc --noEmit && npx vite build)

# 打包：产出 dist/musicReveal.app、dist/musicReveal_<版本>_aarch64.dmg
# 与 dist/musicReveal_<版本>_ffmpeg-lame-source.tar
scripts/package.sh
```

`scripts/package.sh` 依次完成：编译内置 ffmpeg（见下）、生成 Rust 依赖许可清单、`tauri build`、签名、用 `hdiutil` 生成 `.dmg`，最后把内置 ffmpeg 对应的源码打成 `_ffmpeg-lame-source.tar`。可以重复运行。

- **内置 ffmpeg**：`scripts/build-ffmpeg.sh` 从官方地址下载 FFmpeg 与 LAME 源码，校验 SHA-256 后编译一个只含转码所需组件的 LGPL 版本（arm64，macOS 11+，不依赖 Homebrew），结果缓存在 `build/ffmpeg/`，之后复用；脚本内容变了会自动重新编译，传 `--force` 可强制重新编译。
- **签名**：`scripts/sign-local.sh` 第一次运行时用 openssl 生成自签名代码签名证书「musicReveal Local Signing」（有效期 10 年），保存在独立的钥匙串 `~/Library/Keychains/musicreveal-signing.keychain-db`，钥匙串密码保存在同目录的 `musicreveal-signing.password`（权限 600）。之后一直复用这张证书。它不会改动登录钥匙串、钥匙串搜索列表或系统信任设置，全程没有弹窗。删除这两个文件会生成新证书，已经给过的系统授权需要重新授予。

### 发布

发布时必须把 `dist/musicReveal_<版本>_ffmpeg-lame-source.tar` 与 `.dmg` 一起上传到同一个发布页。应用内置的 ffmpeg 以 LGPL 授权，这个 tar 是它的对应源码（未修改的 FFmpeg、LAME 源码包原样，加上构建脚本），按 LGPL 须与二进制在同一处提供；只上传 dmg 不符合许可要求。

仓库中标记为 `#[ignore]` 的测试需要联网或本机真实文件，默认不运行。

## 致谢

QQ 音乐与网易云音乐格式的解密算法参考了以下项目，二者均以 MIT 许可发布：

- [mary20050520/qmunlock](https://github.com/mary20050520/qmunlock)
- [HRuiCcc/music-geshizhuanhuan](https://github.com/HRuiCcc/music-geshizhuanhuan)

## 许可

musicReveal 以 [MIT 许可](LICENSE) 发布。第三方组件（FFmpeg、LAME 及 Rust、npm 依赖）的许可见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

musicReveal 仅用于转换你本人合法获得的音乐文件，请遵守所在地的法律和各音乐平台的服务条款。
