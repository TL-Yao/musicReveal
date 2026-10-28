#!/bin/bash
# 截 musicReveal 主窗口，不需要它在前台，也不要求在当前桌面（Space）：app/scripts/shot.sh <out.png>
# 可用 SHOT_OWNER 换要匹配的应用名（默认 musicreveal）。
set -euo pipefail
out=${1:?用法：shot.sh <out.png>}
src=$(mktemp -t mr-winid).swift
trap 'rm -f "$src" "${src%.swift}"' EXIT
cat > "$src" <<'SWIFT'
import CoreGraphics
import Foundation
let owner = ProcessInfo.processInfo.environment["SHOT_OWNER"] ?? "musicreveal"
let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] ?? []
func area(_ w: [String: Any]) -> Double {
  let r = w[kCGWindowBounds as String] as? [String: Double] ?? [:]
  return (r["Width"] ?? 0) * (r["Height"] ?? 0)
}
let mine = list.filter {
  ($0[kCGWindowOwnerName as String] as? String ?? "").lowercased().contains(owner)
    && ($0[kCGWindowLayer as String] as? Int) == 0
    && area($0) > 10000
}
if let w = mine.max(by: { area($0) < area($1) }), let id = w[kCGWindowNumber as String] { print(id) }
SWIFT
id=$(swift "$src")
[ -n "$id" ] || { echo "找不到 musicReveal 窗口" >&2; exit 1; }
screencapture -l "$id" -o -x "$out"
