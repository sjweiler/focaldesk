#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

test -s CHANGELOG.md
test -s docs/known-issues.md
rg -q '^## Unreleased' CHANGELOG.md
rg -q 'just uninstall' docs/building.md
bash scripts/check-markdown-links.sh
bash scripts/test-install-lifecycle.sh >/dev/null

if [[ -n "${GITHUB_REF_NAME:-}" && "$GITHUB_REF_NAME" != v* ]]; then
    echo "Release ref must begin with v: $GITHUB_REF_NAME" >&2
    exit 1
fi

echo "Release documentation and install lifecycle checks passed."
