# CL-cabi-tests

A test of Accessor's C interface (`accessor.h`) from Common Lisp: SBCL with [CFFI](https://cffi.common-lisp.dev/), loaded through Quicklisp.

```
cargo build -p accessor-capi         # once: ../target/debug/accessor_capi.dll (.so / .dylib elsewhere)
./run.sh                             # or  .\run.ps1  on Windows
```

Set `ACCESSOR_LIBRARY` to use a library somewhere else. The test declares the functions with `defcfun`, passes text as UTF-8 with explicit byte lengths, and checks versions, rows and every value type, prepared statements and all bind kinds, error codes and messages, misuse (NULL handles), transactions, a file database that persists and is locked while open, and a 20,000-row result. Exit code 0 means every check passed.
