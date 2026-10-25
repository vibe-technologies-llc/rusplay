#!/usr/bin/env sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

find "$root/crates" -type f \( -name '*.rs' -o -name '*.wgsl' \) -print0 \
    | xargs -0 perl "$root/scripts/check-comments.pl"
