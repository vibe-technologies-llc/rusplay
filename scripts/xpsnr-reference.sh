#!/bin/bash
set -euo pipefail

directory="${1:?usage: scripts/xpsnr-reference.sh <fixture directory>}"
RUSPLAY_XPSNR_FIXTURE_DIR="$directory" cargo test -q -p rusplay-core write_xpsnr_fixtures -- --ignored >/dev/null

cd "$directory"
while read -r name size format fps; do
  scores=$(ffmpeg -nostdin -hide_banner -loglevel error \
    -f rawvideo -pix_fmt "$format" -s "$size" -framerate "$fps" -i "$name.src.yuv" \
    -f rawvideo -pix_fmt "$format" -s "$size" -framerate "$fps" -i "$name.rec.yuv" \
    -lavfi "[0:v][1:v]xpsnr,metadata=print:file=-" -f null - 2>&1 |
    awk '/^frame:2 /{last=1} last && /lavfi.xpsnr.xpsnr/{split($0, value, "="); printf "%s ", value[2]}')
  echo "$name $scores"
done < cases.txt
