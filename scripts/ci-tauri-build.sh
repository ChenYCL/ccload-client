#!/usr/bin/env bash
# CI 里编壳体。macOS 上 Tauri 的 bundle_dmg.sh 会偶发挂掉（hdiutil Resource
# busy / AppleScript 超时），而那时 .app 已经编完了。整条 matrix 跟着红，
# win/linux 产物也无法发布。失败就卸掉残留映像再试；三次还不行但 .app 在，
# 就靠 zip 发版，别把整次 beta 卡死。
set -euo pipefail

detach_leftover_images() {
  shopt -s nullglob
  local vol
  for vol in /Volumes/ccLoad*; do
    hdiutil detach "$vol" -force >/dev/null 2>&1 || true
  done
}

if [[ "$(uname -s)" != Darwin ]]; then
  exec npx tauri build "$@"
fi

n=0
until npx tauri build "$@"; do
  n=$((n + 1))
  echo "macOS tauri build failed (attempt ${n}) — usually bundle_dmg.sh / hdiutil"
  detach_leftover_images
  if (( n >= 3 )); then
    APP=$(find src-tauri/target -maxdepth 6 -name '*.app' -type d | head -1 || true)
    if [[ -n "${APP}" ]]; then
      echo "::warning::DMG failed 3 times; shipping .app zip from ${APP}"
      exit 0
    fi
    exit 1
  fi
  sleep 8
done
