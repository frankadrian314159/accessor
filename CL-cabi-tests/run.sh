#!/bin/bash
# Runs the Common Lisp test of the C interface: SBCL, CFFI (through Quicklisp) and the native library.
# Build the library first:   cargo build -p accessor-capi     (set ACCESSOR_LIBRARY to use another file)
set -euo pipefail
cd "$(dirname "$0")"
sbcl --noinform --non-interactive --load accessor-cabi-test.lisp
