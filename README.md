# musicReveal

English | [简体中文](README.zh-CN.md)

musicReveal is a small music player for macOS with built-in decryption. It watches folders you choose, converts encrypted downloads from QQ Music and NetEase Cloud Music into ordinary audio files on your machine, fills in cover art, lyrics and other metadata, and plays them from its own library.

<!-- Screenshots: main window (library and tracks being converted), lyrics view, settings. -->

## Features

### Supported formats

| Source | Formats | Notes |
| --- | --- | --- |
| QQ Music (current) | `mflac`, `mgg` and other musicex formats | Requires the QQ Music desktop app installed and logged in on this Mac |
| QQ Music (legacy) | `qmcflac`, `qmcogg`, `qmc0`/`qmc2`/`qmc3`, `bkc*`, and `mflac`/`mgg` files carrying their own key | Tested with synthetic samples only, cross-checked byte for byte against a reference implementation; not verified with real files |
| NetEase Cloud Music | `ncm` | No login required |
| Plain audio | `mp3`, `flac`, `ogg`, `m4a`, `wav` | Copied into the output folder, renamed and enriched like everything else; plain files downloaded from NetEase are enriched by the track ID stored inside them |

### Output

- **Keep original format**: FLAC, OGG, M4A, MP3 and so on stay as they are after decryption. Exceptions: APE and WMA cannot reliably hold full tags, so APE becomes FLAC (lossless to lossless) and WMA becomes 320 kbps MP3.
- **Apple Music compatible**: convert to formats Apple Music and iPhone play natively, using the ffmpeg bundled inside the app (nothing to install). FLAC, WAV and APE become ALAC (`.m4a`, lossless, same bit depth and sample rate); OGG, Opus and WMA become 256 kbps AAC (`.m4a`); MP3 and M4A are left untouched. All tags, cover art and lyrics are preserved.
- Toggling this setting also converts the lossless tracks already in your output folder (FLAC/WAV to ALAC when turned on, ALAC back to FLAC when turned off). You are told how many tracks are affected and roughly how long it will take before anything happens. Tracks are converted one at a time: a hidden copy is written next to the original, verified sample for sample and checked for intact tags, cover and lyrics, and only then swapped in. File creation dates are kept; existing files are never overwritten. Playback keeps working during the conversion; quitting interrupts it and it resumes on next launch.
- When in doubt, nothing is converted: only files musicReveal could have written itself are touched. Files with custom tag fields, multi-valued or empty tags, WAV files with lyrics, more than two channels, 32-bit or floating-point audio, multiple pictures or a non-front-cover picture, and read-only files are left alone and keep playing as they are.

### Metadata enrichment

Existing tags are kept. The track ID inside the file is then used to query the originating platform for year, track and disc numbers, album artist, high-resolution cover art and lyrics (with translation). All artists on a track are retained. Lyrics are embedded as timestamped LRC and also written as a sibling `.lrc` under the `lrc` folder of the output directory, mirroring the sub-path. A `.lrc` sitting next to a plain audio file is picked up too. NetEase's leading JSON credit lines in lyrics are rewritten as ordinary lyric lines.

### Automatic watching

While the app is running (with the window closed as well), newly completed downloads in watched folders are converted automatically; downloads that happened while the app was not running are picked up on next launch. You can also drop files onto the window or use "Add files…" to convert manually. The output folder itself is never treated as a source.

### Library and playback

The library is simply every playable file in the output folder (including sub-folders), including files you put there by hand.

- Play/pause, previous/next, seeking, volume, shuffle, repeat, search and sorting.
- A play queue you can inspect and jump around in.
- Lyrics view with scrolling lyrics (with translation): embedded timestamped lyrics are preferred, falling back to a sibling `.lrc`. On scan, tracks without embedded timestamped lyrics but with a usable `.lrc` get the lyrics embedded, and `.lrc` files next to tracks are moved into the `lrc` folder.
- Keyboard media keys and the Control Center "Now Playing" widget control playback.
- Right-click to reveal in Finder or move to Trash (together with its `.lrc`); ⌘⌫ trashes the current track, ⌘Z undoes.

### Source files are never modified

Converted files go to the output folder (default `~/Music/musicReveal`), named `Artist - Title` (multiple artists joined with "、"). Name collisions get a numeric suffix rather than overwriting. Tracks already converted are skipped, including byte-identical files that were merely renamed or moved.

## Requirements

macOS 11 or later on Apple Silicon (arm64). Only tested on Apple Silicon Macs; Intel Macs and Windows are untested and unsupported.

The user interface is currently Chinese only. Labels quoted below give the on-screen Chinese text in parentheses.

## Installation and first launch

1. Open `musicReveal_<version>_aarch64.dmg` and drag musicReveal into Applications.
2. The first launch has to be allowed manually, see below.

musicReveal is signed with a fixed, self-generated certificate and is not notarized by Apple. The signature itself is valid (`codesign --verify` passes, and tampering with the bundle breaks it), but because the certificate is not issued by Apple, Gatekeeper rejects it and the first launch of a downloaded copy is blocked:

- **macOS 14 and earlier**: Control-click (or right-click) musicReveal in Applications, choose Open, then click Open again in the dialog.
- **macOS 15 and later**: double-click it once and click Done on the warning; then go to System Settings → Privacy & Security, scroll to the message about musicReveal, click Open Anyway and confirm with your login password.

After that it opens normally. Because the certificate never changes, permissions you have granted (such as the one described under FAQ) survive upgrades to new versions signed with the same certificate. A build you make from source carries no quarantine flag and opens directly.

## Usage

1. On first launch, add the folders to watch (you can also add them later from the sidebar).
2. Confirm the output folder and output format (original or Apple Music compatible). Both can be changed later in Settings.
3. If a newly added folder already contains unconverted tracks, the app asks once whether to convert them; otherwise only new downloads are processed.
4. Download music in QQ Music or NetEase Cloud Music as usual. Finished downloads appear under "Converting" (正在转换) above the library and move into the library when done.

Closing the window does not quit: playback and conversion continue, and clicking the Dock icon brings the window back. Press ⌘Q to quit; a track being converted is interrupted, its partial output deleted, and it is converted again on next launch. Settings are under musicReveal → Settings… (⌘,).

## Privacy

- Decryption, transcoding and tagging all happen on your machine. Audio files are never uploaded anywhere.
- Network access is used only for metadata: track info, cover art and lyrics are fetched from the public QQ Music and NetEase Cloud Music endpoints by track ID. There is no fuzzy search and no audio content is sent.
- Decryption keys for QQ Music's current format must be obtained from QQ Music's own endpoint, which requires the login state stored by the QQ Music desktop app on this Mac. That information is used locally for this request only, never stored elsewhere and never sent to a third party. The account shown in the UI is masked.
- NetEase Cloud Music files require no login state at all.

## FAQ

**A track under "Converting" shows "Waiting for login" (待登录)**
It is a QQ Music current-format file, which needs QQ Music's login state, and the QQ Music app on this Mac is not logged in or the session has expired. Open QQ Music and log in; the tracks continue automatically.

**QQ Music status shows "Authorization needed" (需要授权)**
Older versions of QQ Music keep their login state in a system-protected location. Click "Authorize…" (授权…) next to the QQ Music status, enable musicReveal under Privacy & Security → Full Disk Access, then relaunch musicReveal. Updating QQ Music to the latest version removes the need for this.

**Conversion failed with "File incomplete" (文件不完整)**
The file is still downloading, or the download was interrupted and the file is truncated. Wait for the download to finish and click Retry; if the file itself is corrupt, download it again from the platform.

**"ffmpeg not found" when converting formats**
The packaged app bundles ffmpeg. If you are running a development build started with `cargo` or `npx tauri dev`, install ffmpeg yourself (for example `brew install ffmpeg`) or point the `REVEAL_FFMPEG` environment variable at it.

## Building from source

Requires the Xcode Command Line Tools, stable Rust, Node.js 22, and `cargo-about` for generating the dependency license inventory (`cargo install --locked cargo-about`).

```sh
# all commands run from the repository root

# development
(cd app && npm ci && npx tauri dev)

# checks and tests
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
(cd app && npx tsc --noEmit && npx vite build)

# packaging: produces dist/musicReveal.app, dist/musicReveal_<version>_aarch64.dmg
# and dist/musicReveal_<version>_ffmpeg-lame-source.tar
scripts/package.sh
```

`scripts/package.sh` builds the bundled ffmpeg, generates the Rust dependency license inventory, runs `tauri build`, signs the app, creates the `.dmg` with `hdiutil`, and finally packs the source corresponding to the bundled ffmpeg into `_ffmpeg-lame-source.tar`. It is safe to run repeatedly.

- **Bundled ffmpeg**: `scripts/build-ffmpeg.sh` downloads the FFmpeg and LAME sources from their official sites, verifies SHA-256 checksums, and builds an LGPL-only ffmpeg containing just the components needed for transcoding (arm64, macOS 11+, no Homebrew dependency). The result is cached in `build/ffmpeg/`; the script rebuilds automatically when its own contents change, or on demand with `--force`.
- **Signing**: on first run, `scripts/sign-local.sh` uses openssl to create a self-signed code-signing certificate named "musicReveal Local Signing" (valid for 10 years) in a dedicated keychain at `~/Library/Keychains/musicreveal-signing.keychain-db`, with the keychain password stored alongside in `musicreveal-signing.password` (mode 600). It does not touch the login keychain, the keychain search list or system trust settings, and shows no prompts. Deleting those two files creates a fresh certificate, after which previously granted system permissions must be granted again.

### Repository layout

| Path | Contents |
| --- | --- |
| `crates/reveal-core` | Core library: format detection, decryption, metadata enrichment, transcoding, library |
| `app/src-tauri` | Tauri backend: folder watching, conversion queue, player, system integration |
| `app/src` | Frontend (TypeScript, no framework) |
| `scripts/` | Packaging, bundled ffmpeg build, local signing, license inventory |
| `tools/crosscheck` | Developer tool: byte-for-byte cross-check of decryption output against a reference implementation using synthetic samples |

### Tests

Tests marked `#[ignore]` need network access or real files and are skipped by default. Real samples (QQ Music `.mflac`, NetEase `.ncm`, and `.mp3` files carrying a 163 key) live in `testdata/real-samples/`; they are copyrighted music, listed in `.gitignore` and never committed. `REVEAL_NCM_DIR` / `REVEAL_E2E_DIR` point the tests elsewhere. Tests copy samples to a temporary directory before use; the end-to-end `.mflac` test additionally reads the local QQ Music login state.

Before a release, run the whole suite once more against the bundled, trimmed-down ffmpeg: a full ffmpeg on the development machine would hide muxers and decoders the bundled build lacks.

```sh
scripts/package.sh
REVEAL_FFMPEG=$PWD/dist/musicReveal.app/Contents/MacOS/ffmpeg \
REVEAL_GEN_FFMPEG=/opt/homebrew/bin/ffmpeg \
cargo test --workspace -- --include-ignored
```

### Releasing

`dist/musicReveal_<version>_ffmpeg-lame-source.tar` must be uploaded to the same release page as the `.dmg`. The bundled ffmpeg is LGPL-licensed, and this tarball is its corresponding source, which the LGPL requires to be offered alongside the binary. Publishing the dmg alone does not satisfy the license.

## License

musicReveal is released under the [MIT License](LICENSE). Licenses of third-party components and reference implementations are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

musicReveal is intended only for converting music files you have lawfully obtained. Please comply with the laws of your jurisdiction and the terms of service of the music platforms you use.
