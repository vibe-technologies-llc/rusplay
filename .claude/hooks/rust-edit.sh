#!/usr/bin/env sh
set -u

root=${CLAUDE_PROJECT_DIR:-$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)}

file=$(perl -MJSON::PP -e '
    local $/;
    my $input = decode_json(<STDIN>);
    print $input->{tool_input}{file_path} // "";
')

case "$file" in
    *.rs) rust-formatter --quiet "$file" >/dev/null 2>&1 ;;
    *.wgsl) ;;
    *) exit 0 ;;
esac

if ! report=$(perl "$root/scripts/check-comments.pl" "$file"); then
    printf 'Comments are banned in this project (see .claude/rules/style.md). Move what they say into names, types or docs:\n%s\n' "$report" >&2
    exit 2
fi
