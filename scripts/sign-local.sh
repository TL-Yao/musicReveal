#!/usr/bin/env bash
# 用固定的自签名证书签名 musicReveal.app。
# 证书放在独立钥匙串 ~/Library/Keychains/musicreveal-signing.keychain-db 里，第一次运行时创建，
# 之后一直复用：签名身份不变，macOS 已给出的授权（如访问 QQ 音乐数据）在重新构建后仍然有效。
# 不改 login 钥匙串、不改钥匙串搜索列表、不改系统信任设置，全程不弹窗。
# 用法：scripts/sign-local.sh <path/to/musicReveal.app>
set -euo pipefail

APP="${1:?用法：$0 <musicReveal.app>}"
CN="musicReveal Local Signing"
KC="$HOME/Library/Keychains/musicreveal-signing.keychain-db"
PWFILE="$HOME/Library/Keychains/musicreveal-signing.password"
BUNDLE_ID="io.github.tlyao.musicreveal"

search_list() { security list-keychains -d user; }
LIST_BEFORE="$(search_list)"

if [[ ! -f "$KC" ]]; then
  echo "创建签名钥匙串：$KC"
  (umask 077 && /usr/bin/openssl rand -hex 32 >"$PWFILE")
  security create-keychain -p "$(cat "$PWFILE")" "$KC"
  security set-keychain-settings "$KC" # 不自动上锁
fi
[[ "$(stat -f %Lp "$PWFILE")" == 600 ]] || chmod 600 "$PWFILE"
PW="$(cat "$PWFILE")"
security unlock-keychain -p "$PW" "$KC"

if ! security find-certificate -c "$CN" "$KC" >/dev/null 2>&1; then
  echo "生成自签名代码签名证书：$CN"
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  P12PW="$(/usr/bin/openssl rand -hex 16)"
  /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 \
    -keyout "$TMP/key.pem" -out "$TMP/cert.pem" -subj "/CN=$CN" \
    -addext "basicConstraints=critical,CA:false" \
    -addext "keyUsage=critical,digitalSignature" \
    -addext "extendedKeyUsage=critical,codeSigning" 2>/dev/null
  # 用系统自带的 LibreSSL 导出：它默认的 p12 加密算法 security import 能直接读。
  /usr/bin/openssl pkcs12 -export -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
    -name "$CN" -out "$TMP/id.p12" -passout "pass:$P12PW"
  security import "$TMP/id.p12" -k "$KC" -P "$P12PW" -T /usr/bin/codesign >/dev/null
  # 允许 codesign 无提示使用私钥。
  security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$PW" "$KC" >/dev/null
fi

SHA1="$(security find-certificate -c "$CN" -Z "$KC" | awk '/^SHA-1 hash:/{print $3}')"
[[ -n "$SHA1" ]] || { echo "找不到签名证书" >&2; exit 1; }

sign() { codesign --force --sign "$SHA1" --keychain "$KC" --options runtime --timestamp=none "$@"; }
# 先签内部可执行文件，再签整个 app；不用 --deep 签名，保证各自的标识符确定。
if [[ -f "$APP/Contents/MacOS/ffmpeg" ]]; then
  sign --identifier "$BUNDLE_ID.ffmpeg" "$APP/Contents/MacOS/ffmpeg"
fi
sign "$APP"

codesign --verify --deep --strict --verbose=2 "$APP"
[[ "$(search_list)" == "$LIST_BEFORE" ]] || echo "警告：钥匙串搜索列表发生了变化" >&2
echo "签名证书 SHA-1：$SHA1"
