# Several processes and one database file

**A database file is open in one process at a time.** Within that process any number of threads can use it at once ([concurrency](../concurrency.md)); a second process that tries to open the same file is refused at once:

```
the database '/data/shop.adb' is in use by another connection or process
```

(`Error::Locked` in Rust, `ACC_ERR_LOCKED` in C, SQL state `08001` in JDBC.)

| Who opens the file | Result |
| --- | --- |
| the first program, any API (Rust, shell, GUI, C, JDBC) | succeeds, and holds the file until it closes it or ends |
| a second program, while the first has it open | `Locked`; nothing is read, nothing is changed |
| a second open **in the same process** with `Database::open` or `SharedDatabase::open` | `Locked` too: share the first with `SharedDatabase::connect()` |
| another thread of the first process, through a `SharedDatabase` | succeeds: it is the same open, with overlapping transactions |
| a second program, once the first has closed it or died | succeeds |

## Why

The engine keeps the whole database in memory and appends every commit to a write-ahead log. Two processes doing that to the same file would each be unaware of the other's changes, and the second to write would overwrite the first. Rather than leave that to be avoided, the engine refuses it. (Threads of one process are different: they share the one copy in memory, and the engine arranges their transactions.)

## How the lock works

Opening a database takes an **exclusive operating-system file lock** on a small file beside it, `name.adb-lock`. (The lock is on a separate file because the database file itself is replaced, by rename, at every checkpoint, and a lock does not follow a renamed file.)

* The lock is taken **before** anything is read, so a refused open changes nothing, and does not disturb the process that has the file.
* It is taken with `flock` on Unix and `LockFileEx` on Windows, so it works between processes and between opens in the same process.
* It is released when the database is closed, **and by the operating system when the process ends**, however it ends: a crash, a kill or a power cut never leaves a database locked. There is no stale lock to clean up, and the `-lock` file left on disk is harmless (it is empty).
* Different database files do not affect each other, and in-memory databases need no lock.
* A file system that cannot lock (some network shares) gives an "unsupported" answer; the database then opens **unlocked**. Do not share a database on such a file system between programs.
* It does not stop a program that ignores the convention from opening the `.adb` file with some other tool.

## Using one database from several programs

There is no mode in which two processes use the file at the same moment. Ways to get the same effect:

* **One program owns the database, the others ask it.** The owner opens a `SharedDatabase` and serves requests over a socket, a pipe or HTTP, each request on a connection of its own; the others never touch the file. This is the way to have several writers from several processes, and it is what a client/server database does for you. Accessor does not include the server.
* **Take turns.** Programs that run one after another (a nightly import, then a report) just open and close the file. A program that may find it busy retries: `Locked` means "somebody has it", and it is gone as soon as they close it.
  ```rust
  let db = loop {
      match Database::open(path) {
          Err(Error::Locked(_)) => std::thread::sleep(std::time::Duration::from_millis(200)),
          other => break other?,
      }
  };
  ```
* **Read a copy.** A program that only needs to look at the data can open a copy of the file taken while the owner was closed, or after a checkpoint (see below).
* **Export.** `.export` and `.import` in the shell (CSV) move data between databases without sharing a file.

## What a crash leaves

If a process dies, the file is unlocked at once. The next program to open it replays the write-ahead log: every transaction that had committed is there, in the order it committed, and none that had not (a checkpoint in the middle of other transactions' work makes no difference). See [concurrency](../concurrency.md#files-durability-and-processes).

## Backups

Copy a database while it is open only if you copy the `.adb` and `-wal` files **together**; or close it first, which folds the log into the snapshot. A checkpoint (`.checkpoint` in the shell, `Connection::checkpoint`, `acc_checkpoint`) folds the log in without closing, after which the `.adb` file alone is complete as of that moment. The lock does not stop other tools copying the files, but a copy of the `.adb` alone taken between checkpoints misses the transactions that are only in the log.

## Tests

`crates/accessor-core/tests/persistence.rs` opens a file twice, from the same process and after a simulated crash; `tests/shared.rs` checks that a `SharedDatabase` is one open; and the C test suite starts a **second process** that tries to open the file while the first has it, and checks that it is refused and that the first is undisturbed.
