#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
status=0

while IFS= read -r -d '' document; do
    document_dir=$(dirname "$document")

    while IFS= read -r match; do
        target=${match#](}
        target=${target%)}
        target=${target#<}
        target=${target%>}

        case "$target" in
            ""|\#*|http://*|https://*|mailto:*)
                continue
                ;;
        esac

        target=${target%%#*}
        candidate="$document_dir/$target"
        if [[ ! -e "$candidate" ]]; then
            printf '%s: missing local Markdown target: %s\n' \
                "${document#"$repo_root"/}" "$target" >&2
            status=1
        fi
    done < <(grep -oE '\]\([^)]+\)' "$document" || true)
done < <(find "$repo_root" -type f -name '*.md' \
    -not -path "$repo_root/target/*" -print0)

exit "$status"
