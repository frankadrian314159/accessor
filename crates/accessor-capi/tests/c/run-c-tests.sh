#!/bin/bash
# Builds the C interface and runs the C tests: plainly, under Valgrind, and with AddressSanitizer +
# UndefinedBehaviorSanitizer.
#
#   run-c-tests.sh [plain] [valgrind] [asan]      (default: all three)
#
# Needs: cargo, a C compiler ($CC, default "cc") and, for the modes asked for, valgrind and a nightly
# Rust toolchain (rustup toolchain install nightly) with GCC's or clang's sanitizer runtimes.
# Build output goes to $CARGO_TARGET_DIR (default: target/).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
crate="$(cd "$here/../.." && pwd)"
root="$(cd "$crate/../.." && pwd)"
CC="${CC:-cc}"
target_dir="${CARGO_TARGET_DIR:-$root/target}"
out="${TMPDIR:-/tmp}/accessor-c-tests"
mkdir -p "$out"
modes=("$@")
[ ${#modes[@]} -eq 0 ] && modes=(plain valgrind asan)

cflags=(-std=c11 -Wall -Wextra -Werror -pedantic -g -O1 -I"$crate/include")
libs=(-lpthread -ldl -lm)

build_lib() { # build_lib <label> [extra cargo args...]
    local label="$1"; shift
    (cd "$root" && cargo build -p accessor-capi "$@")
}

# canary <description> <command...>: the command must fail, or the checker is not working.
canary() {
    local what="$1"; shift
    if "$@" >"$out/canary.log" 2>&1; then
        echo "$what did NOT catch the deliberate overflow in canary.c: the check cannot be trusted" >&2
        cat "$out/canary.log" >&2
        exit 3
    fi
    echo "canary: $what caught the deliberate overflow, as it must"
}

for mode in "${modes[@]}"; do
    echo "=== $mode ==="
    case "$mode" in
    plain)
        build_lib plain
        "$CC" "${cflags[@]}" "$here/test_accessor.c" "$target_dir/debug/libaccessor_capi.a" "${libs[@]}" -o "$out/test-plain"
        "$out/test-plain"
        ;;
    valgrind)
        build_lib valgrind
        "$CC" "${cflags[@]}" "$here/test_accessor.c" "$target_dir/debug/libaccessor_capi.a" "${libs[@]}" -o "$out/test-valgrind"
        "$CC" -g -O0 "$here/canary.c" -o "$out/canary-valgrind"
        canary valgrind valgrind --quiet --error-exitcode=99 "$out/canary-valgrind"
        # --error-exitcode makes any memory error, invalid access or definite/possible leak fail the run.
        valgrind --quiet --error-exitcode=99 --leak-check=full --show-leak-kinds=definite,possible \
            --errors-for-leak-kinds=definite,possible --track-origins=yes --num-callers=30 \
            "$out/test-valgrind"
        ;;
    asan)
        # The library itself is instrumented, as well as the C test, so overflows inside the Rust
        # code that handles C pointers are caught too.
        (cd "$root" && RUSTFLAGS="-Zsanitizer=address -Cdebuginfo=1" \
            cargo +nightly build -p accessor-capi --target x86_64-unknown-linux-gnu --target-dir "$target_dir/asan")
        "$CC" "${cflags[@]}" -fsanitize=address,undefined -fno-omit-frame-pointer -fno-sanitize-recover=undefined \
            "$here/test_accessor.c" "$target_dir/asan/x86_64-unknown-linux-gnu/debug/libaccessor_capi.a" "${libs[@]}" -o "$out/test-asan"
        "$CC" -g -O0 -fsanitize=address,undefined -fno-omit-frame-pointer "$here/canary.c" -o "$out/canary-asan"
        canary AddressSanitizer "$out/canary-asan"
        ASAN_OPTIONS="detect_leaks=1:abort_on_error=0:detect_stack_use_after_return=1:strict_string_checks=1:check_initialization_order=1" \
        UBSAN_OPTIONS="print_stacktrace=1:halt_on_error=1" \
            "$out/test-asan"
        ;;
    *)
        echo "unknown mode '$mode' (plain, valgrind, asan)" >&2
        exit 2
        ;;
    esac
done
echo "all requested C test modes passed"
