#!/usr/bin/env bash
# Use the unpublished sibling FocalVector client without changing the pinned
# upstream dependency for normal/CI builds. Pass ordinary cargo arguments.
set -euo pipefail
repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
vector_root=$(cd -- "$repo_root/../focal-vector" && pwd)
cd -- "$repo_root"
exec cargo --config "patch.\"https://github.com/sjweiler/focal-vector\".focal-vector-client.path=\"$vector_root/crates/focal-vector-client\"" "$@"
