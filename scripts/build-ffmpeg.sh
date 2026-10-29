#!/usr/bin/env bash
# 构建随 musicReveal 分发的精简 ffmpeg：LGPL、arm64、macOS 11+，静态链接 LAME。
# 产物：build/ffmpeg/out/ffmpeg 与 build/ffmpeg/out/licenses/。已存在且由本脚本的当前内容
# 构建（含全部 configure 参数，改了就重建）则直接复用，传 --force 重新构建。
# 源码包缓存在 build/ffmpeg/dl/，校验 SHA-256 后才使用。产物缺 App 要用的 muxer 时终止。
set -euo pipefail

FFMPEG_VER=9.0.2
FFMPEG_SHA256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
FFMPEG_URL="https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VER}.tar.xz"
LAME_VER=3.100
LAME_SHA256=ddfe36cab873794038ae2c1210557ad34857a4b6bdc515785d1da9e175b1da1e
LAME_URL="https://downloads.sourceforge.net/project/lame/lame/${LAME_VER}/lame-${LAME_VER}.tar.gz"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SELF="$ROOT/scripts/build-ffmpeg.sh"
BASE="$ROOT/build/ffmpeg"
DL="$BASE/dl"
OUT="$BASE/out"
WORK="$BASE/work"
# App 用到的 muxer：转码（mp3、flac）、抽封面（image2pipe）、补嵌歌词前核对（streamhash）
MUXERS=(mp3 flac image2pipe streamhash)

check_muxers() { # ffmpeg 路径
  local have
  have="$("$1" -hide_banner -muxers 2>/dev/null | awk '$1 ~ /E/ {print $2}')"
  local m
  for m in "${MUXERS[@]}"; do
    grep -qx "$m" <<<"$have" || { echo "$1 缺少 muxer：$m，终止" >&2; return 1; }
  done
}

if [[ "${1:-}" != "--force" && -x "$OUT/ffmpeg" && -f "$OUT/licenses/SOURCE.md" ]] \
  && cmp -s "$SELF" "$OUT/licenses/build-ffmpeg.sh"; then
  check_muxers "$OUT/ffmpeg"
  echo "ffmpeg 已构建：$OUT/ffmpeg"
  exit 0
fi

fetch() { # url sha256 file
  local f="$DL/$3"
  if [[ ! -f "$f" ]] || ! echo "$2  $f" | shasum -a 256 -c --status; then
    echo "下载 $1"
    curl -fL --retry 3 -o "$f.part" "$1"
    mv "$f.part" "$f"
  fi
  echo "$2  $f" | shasum -a 256 -c
}

mkdir -p "$DL"
fetch "$FFMPEG_URL" "$FFMPEG_SHA256" "ffmpeg-${FFMPEG_VER}.tar.xz"
fetch "$LAME_URL" "$LAME_SHA256" "lame-${LAME_VER}.tar.gz"

rm -rf "$WORK" "$OUT"
mkdir -p "$WORK/prefix" "$OUT/licenses"
trap 'rm -rf "$WORK"' EXIT
PREFIX="$WORK/prefix"
JOBS="$(sysctl -n hw.ncpu)"

export MACOSX_DEPLOYMENT_TARGET=11.0
export CC=clang
export PKG_CONFIG_LIBDIR="$PREFIX/lib/pkgconfig"
unset PKG_CONFIG_PATH LIBRARY_PATH CPATH C_INCLUDE_PATH
ARCHFLAGS="-arch arm64 -mmacosx-version-min=11.0"

tar -xzf "$DL/lame-${LAME_VER}.tar.gz" -C "$WORK"
(
  cd "$WORK/lame-${LAME_VER}"
  # -w：LAME 3.100 在新版 clang 下有大量无害告警。
  CFLAGS="$ARCHFLAGS -O2 -w" LDFLAGS="$ARCHFLAGS" ./configure \
    --prefix="$PREFIX" --host=aarch64-apple-darwin --build=aarch64-apple-darwin \
    --disable-shared --enable-static --disable-frontend --disable-decoder \
    --disable-dependency-tracking >/dev/null
  make -j"$JOBS" >/dev/null
  make install >/dev/null
)

tar -xJf "$DL/ffmpeg-${FFMPEG_VER}.tar.xz" -C "$WORK"
cd "$WORK/ffmpeg-${FFMPEG_VER}"
# 路径用相对形式（../prefix），以免本机目录名写进 ffmpeg -version 的输出。
CONF=(
  --arch=arm64 --target-os=darwin --cc=clang
  --extra-cflags="$ARCHFLAGS -I../prefix/include"
  --extra-ldflags="$ARCHFLAGS -L../prefix/lib"
  --disable-everything --disable-autodetect --disable-network
  --disable-doc --disable-debug --disable-programs --enable-ffmpeg
  --disable-avdevice --disable-swscale
  --enable-zlib --enable-libmp3lame
  --enable-protocol=file,pipe
  --enable-demuxer=flac,ogg,mp3,mov,aac,wav,asf,ape
  # png / mjpeg：封面流要能读出尺寸，streamhash 才肯核对
  --enable-decoder=flac,vorbis,opus,mp3,mp3float,aac,alac,pcm_s16le,pcm_s24le,pcm_s32le,pcm_f32le,pcm_u8,wmav1,wmav2,ape,png,mjpeg
  --enable-parser=flac,vorbis,opus,mpegaudio,aac
  --enable-encoder=libmp3lame,flac
  --enable-muxer="$(IFS=,; echo "${MUXERS[*]}")"
  --enable-filter=abuffer,abuffersink,aformat,anull,aresample,atrim
)
./configure "${CONF[@]}" \
  | tee "$WORK/configure.log" >/dev/null
grep -q '^License: LGPL' "$WORK/configure.log" || { grep '^License' "$WORK/configure.log"; echo "许可证不是 LGPL，终止" >&2; exit 1; }
make -j"$JOBS" ffmpeg >/dev/null
strip -x ffmpeg -o "$OUT/ffmpeg"
check_muxers "$OUT/ffmpeg" || { rm -rf "$OUT"; exit 1; }

if otool -L "$OUT/ffmpeg" | tail -n +2 | grep -vE '^\s+/(usr/lib|System)/'; then
  echo "ffmpeg 链接了系统之外的动态库，终止" >&2
  exit 1
fi

cp COPYING.LGPLv2.1 "$OUT/licenses/ffmpeg-COPYING.LGPLv2.1"
cp LICENSE.md "$OUT/licenses/ffmpeg-LICENSE.md"
cp "$WORK/lame-${LAME_VER}/COPYING" "$OUT/licenses/LAME-COPYING"
cp "$SELF" "$OUT/licenses/build-ffmpeg.sh"
LICENSE_LINE="$(grep -o '^License:.*' "$WORK/configure.log")"
cat >"$OUT/licenses/SOURCE.md" <<EOF
# 随附 ffmpeg 的来源说明

musicReveal.app 内的 \`Contents/MacOS/ffmpeg\` 是由 FFmpeg ${FFMPEG_VER} 与 LAME ${LAME_VER}
（静态链接）编译而成的独立可执行文件，未启用任何 GPL 或 nonfree 组件（configure 输出：${LICENSE_LINE}）。
它以独立进程方式被 musicReveal 调用，用于把音频转码为 MP3 / FLAC、从 WMA / APE 中原样取出封面，
以及在把歌词写进音乐文件前后核对其余元数据没有变化。

对应源码随发布一同提供：与 dmg 放在一起的 \`musicReveal_<版本>_ffmpeg-lame-source.tar\`，
内含下列两个源码包原样（未打补丁，位于 build/ffmpeg/dl/）与构建脚本 scripts/build-ffmpeg.sh。
解包后无需联网即可重建（需要 Xcode 命令行工具，Apple Silicon Mac）：

\`\`\`
tar -xf musicReveal_<版本>_ffmpeg-lame-source.tar
cd musicReveal_<版本>_ffmpeg-lame-source
scripts/build-ffmpeg.sh        # 产物：build/ffmpeg/out/ffmpeg
\`\`\`

源码包也可从上游下载，并按 SHA-256 校验与本版本一致：

- FFmpeg 源码 ffmpeg-${FFMPEG_VER}.tar.xz：${FFMPEG_URL}
  SHA-256：${FFMPEG_SHA256}
- LAME 源码 lame-${LAME_VER}.tar.gz：${LAME_URL}
  SHA-256：${LAME_SHA256}
- 构建脚本：build-ffmpeg.sh（应用包 licenses/ 下与源码 tar 的 scripts/ 下各有一份，即仓库中的 scripts/build-ffmpeg.sh），包含全部 configure 参数。

FFmpeg 以 GNU LGPL 2.1 或更高版本授权，许可全文见 ffmpeg-COPYING.LGPLv2.1 与 ffmpeg-LICENSE.md；
LAME 以 GNU LGPL 2 或更高版本授权，许可全文见 LAME-COPYING。
你可以用上述源码和构建脚本自行编译，并替换 .app 中的 ffmpeg（替换后需重新签名）。

完整 configure 参数：
\`\`\`
$(printf '%s\n' "${CONF[@]}")
\`\`\`
EOF
echo "完成：$OUT/ffmpeg"
