#!/usr/bin/env sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

find "$root" \( -name target -o -name .git \) -prune -o -type f \
    \( -name '*.rs' -o -name '*.wgsl' -o -name '*.toml' \) -print0 \
    | xargs -0 perl "$root/scripts/check-comments.pl"
