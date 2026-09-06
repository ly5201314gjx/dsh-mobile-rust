#!/bin/bash
set -u
INDEX=/tmp/pkgs.idx
OUT=/workspace/dsh-mobile-rust/debs
BASE=https://packages.termux.dev/apt/termux-main
cd "$OUT"
for p in nodejs bash ripgrep libandroid-support libiconv readline ncurses pcre2 libc++ openssl c-ares libicu libsqlite zlib libffi ca-certificates; do
  fn=$(awk 'index($0,"Package: '$p'"){f=1} f&&/^Filename: /{print $2; exit}' "$INDEX")
  if [ -z "$fn" ]; then echo "NO-FILENAME: $p"; continue; fi
  base=$(basename "$fn")
  if [ -s "$base" ]; then echo "skip-exists: $base"; continue; fi
  echo "downloading: $base"
  curl -fsSL --max-time 300 -o "$base" "$BASE/$fn" || { echo "FAIL: $base"; rm -f "$base"; }
done
echo "ALL-DONE"
ls -la