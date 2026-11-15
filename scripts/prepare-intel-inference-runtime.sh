#!/usr/bin/env bash
# Fedora x86_64: prepare user-space compute libraries without installing RPMs.
set -euo pipefail
project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
bench_root=${1:-"$project_root/tmp/intel-inference"}
mkdir -p "$bench_root/packages" "$bench_root/runtime"
bench_root=$(realpath "$bench_root")
dnf download --destdir "$bench_root/packages" --resolve --alldeps \
    intel-opencl.x86_64 intel-igc.x86_64 intel-gmmlib.x86_64
for package in "$bench_root"/packages/*.rpm; do
    rpm2cpio "$package" | (cd "$bench_root/runtime" && cpio -idm --quiet)
done
libdir="$bench_root/runtime/usr/lib64"
# Fedora normally creates these SONAME links in package installation triggers.
for library in libigc libigdfcl libiga64; do
    candidates=("$libdir/$library".so.2.*)
    if [[ ${#candidates[@]} != 1 || ! -f ${candidates[0]} ]]; then
        echo "Cannot resolve $library SONAME; inspect downloaded package versions" >&2
        exit 1
    fi
    ln -sf "${candidates[0]##*/}" "$libdir/$library.so.2"
done
llvm_library=$(find "$libdir" -name 'libLLVM.so.*' -type f -print -quit)
if [[ -z $llvm_library ]]; then
    echo 'Missing extracted LLVM runtime' >&2
    exit 1
fi
uv venv --python 3.12 "$bench_root/venv"
uv pip install --python "$bench_root/venv/bin/python" \
    -r "$project_root/scripts/intel-inference-requirements.txt"
{
    printf 'export OCL_ICD_FILENAMES=%q\n' "$libdir/intel-opencl/libigdrcl.so"
    printf 'export LD_LIBRARY_PATH=%q:"${LD_LIBRARY_PATH:-}"\n' "$libdir:$(dirname "$llvm_library")"
    printf 'export VK_DRIVER_FILES=%q\n' '/usr/share/vulkan/icd.d/intel_icd.x86_64.json'
    printf 'export GSK_RENDERER=vulkan\n'
} > "$bench_root/environment.sh"
echo "Ready. Source $bench_root/environment.sh in the benchmark shell only."
