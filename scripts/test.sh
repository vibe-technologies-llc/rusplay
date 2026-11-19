#!/bin/bash
set -uo pipefail

skip_log="$(mktemp)"
trap 'rm -f "$skip_log"' EXIT

RUSPLAY_SKIP_LOG="$skip_log" cargo nextest run "$@"
status=$?

if [[ -s "$skip_log" ]]; then
    printf '\nskipped at run time (%s):\n' "$(wc -l < "$skip_log")" >&2
    sort "$skip_log" >&2
fi

exit "$status"
