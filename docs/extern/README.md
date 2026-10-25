# Using Accessor from other languages

Accessor is a Rust library, but it can be used from any language that can call C, and from Java through JDBC.

| Page | Contents |
| --- | --- |
| [c.md](c.md) | the C interface: `accessor.h`, the library, every function, error codes, threading and memory rules |
| [jdbc.md](jdbc.md) | the JDBC driver: URLs, types, transactions, metadata, what is and is not supported |
| [testing.md](testing.md) | how the C interface is checked: the C test suite, Valgrind, AddressSanitizer, fuzzing |
| [locking.md](locking.md) | several processes and one database file: the lock, patterns for sharing, crashes, backups |

```
 your program ──▶ accessor.h / libaccessor_capi ──▶ accessor-core (the engine, in Rust)
 Java program ──▶ JDBC driver ──(Java foreign-function API)──▶ the same library
```

## Versions

Three numbers describe what you are running, and each can be asked for:

| Call | Meaning | Changes when |
| --- | --- | --- |
| `acc_version()` | the library version, `"0.1.0"` | every release |
| `acc_version_number()` | the same as a number, `major * 10000 + minor * 100 + patch` (so 0.1.0 is 100) | every release |
| `acc_abi_version()` | the version of the C interface | an existing function **changes meaning**; adding functions does not change it |
| `acc_file_format_version()` | the version of the database file format this library writes (currently 5) | the file layout changes; older files are still read and are upgraded on first open, but a file written by this library **cannot be read by an older one** |

A program built against ABI version 1 works with any library that reports 1. Check it at start-up if the library is loaded at run time:

```c
if (acc_abi_version() != 1) { fprintf(stderr, "wrong Accessor library\n"); exit(1); }
```

The JDBC driver does the same when it loads the library.
