# One connection per database file

A database file can be open in **one connection at a time**, in one process or across processes. A second attempt to open it fails at once:

```
the database '/data/shop.adb' is in use by another connection or process
```

(`Error::Locked` in Rust, `ACC_ERR_LOCKED` in C, SQL state `08001` in JDBC.)

## Why

The engine keeps the whole database in memory and appends every commit to a write-ahead log. Two programs doing that to the same file would each be unaware of the other's changes, and the second to write would overwrite the first. Rather than leave that to be avoided, the engine refuses it.

## How it works

Opening a database takes an **exclusive operating-system file lock** on a small file beside it, `name.adb-lock`. (The lock is on a separate file because the database file itself is replaced, by rename, at every checkpoint, and a lock does not follow a renamed file.) The lock is released when the connection is closed, **and by the operating system when the process ends**, however it ends: a crash, a kill or a power cut never leaves a database locked. There is no stale lock to clean up, and the `-lock` file left on disk is harmless (it is empty).

* The lock is taken **before** anything is read, so a refused open changes nothing.
* It is taken with `flock` on Unix and `LockFileEx` on Windows, so it works between processes and between connections in the same process.
* Different database files do not affect each other.
* In-memory databases need no lock.
* A file system that cannot lock (some network shares) gives an "unsupported" answer; the database then opens **unlocked**. Do not share a database on such a file system between programs.

## What it does not do

It does not allow several processes. There is no shared mode between processes: if two programs must read the same data, one should own the database and the other ask it. Within one process, threads share a database through `SharedDatabase` and its connections, with overlapping transactions ([concurrency](../concurrency.md)): they are one open, and so share the one lock. And it does not stop a program that ignores the convention from opening the `.adb` file with another tool.

## Backups

Copy a database while it is open only if you copy the `.adb` and `-wal` files **together**; or close it first, which folds the log into the snapshot. Closing releases the lock.
