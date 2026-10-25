#!/bin/bash
# Fuzzes the C interface with libFuzzer under AddressSanitizer.
#
#   run-fuzz.sh [seconds per target] [target ...]        (default: 60 seconds, both targets)
#
# Needs a nightly Rust toolchain and cargo-fuzz (cargo install cargo-fuzz), and Linux.
# Any crash, sanitizer report, panic, timeout or out-of-memory stops the run and leaves the input in
# fuzz/artifacts/<target>/; reproduce it with:  cargo +nightly fuzz run <target> <that file>
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
crate="$(cd "$here/.." && pwd)"
seconds="${1:-60}"
shift || true
targets=("$@")
[ ${#targets[@]} -eq 0 ] && targets=(strings binds)

python3 "$here/make_seeds.py" >/dev/null
cd "$crate"
for t in "${targets[@]}"; do
    echo "=== fuzzing $t for ${seconds}s ==="
    mkdir -p "$here/corpus/$t"
    cargo +nightly fuzz run "$t" "$here/corpus/$t" "$here/seeds/$t" -- \
        -max_total_time="$seconds" -max_len=4096 -timeout=10 -rss_limit_mb=3072 -dict="$here/sql.dict" \
        -print_final_stats=1 -use_value_profile=1
done
echo "no crashes found"
