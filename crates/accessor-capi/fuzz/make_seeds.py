#!/usr/bin/env python3
"""Writes the seed inputs for the fuzz targets into seeds/<target>/.

Seeds are valid statements, so the fuzzer starts inside the parser and mutates from there instead
of having to discover the grammar. The first byte of a `strings` input chooses the call (see
fuzz_targets/strings.rs): 0 = exec with a length, 1 = exec NUL-terminated, 2 = prepare and run,
3 = report, 4 = parameter lookup, 5-7 = bound text, 8 = cut SQL.
"""
import struct
from pathlib import Path

HERE = Path(__file__).resolve().parent

SQL = [
    "SELECT 1",
    "SELECT i, s FROM t WHERE i > 0 ORDER BY s DESC LIMIT 5",
    "SELECT COUNT(*), SUM(d), AVG(dec) FROM t GROUP BY b HAVING COUNT(*) > 0",
    "SELECT a.i, b.s FROM t a LEFT JOIN t b ON a.i = b.i WHERE a.s LIKE 'o*'",
    "SELECT * FROM t WHERE EXISTS (SELECT 1 FROM t x WHERE x.i = t.i)",
    "SELECT (SELECT MAX(i) FROM t x WHERE x.s = t.s) FROM t",
    "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 5) SELECT n FROM c",
    "SELECT ROW_NUMBER() OVER (PARTITION BY b ORDER BY i), RANK() OVER w FROM t WINDOW w AS (ORDER BY d)",
    "SELECT js -> 'a', JSON_VALUE(js, '$.a[0]'), JSON_SET(js, '$.b', 1) FROM t",
    "SELECT Format(d, '0.00'), Format(dt, 'yyyy-mm-dd'), CAST(i AS TEXT), CAST(s AS INTEGER) FROM t",
    "SELECT REGEXP_REPLACE(s, '(o)(n)', '$2$1'), s SIMILAR TO 'o%', INITCAP(s), TRANSLATE(s, 'on', 'ab') FROM t",
    "SELECT * FROM t PIVOT (SUM(i) FOR b IN (TRUE, FALSE))",
    "TRANSFORM SUM(i) SELECT s FROM t GROUP BY s PIVOT b",
    "SELECT dt + INTERVAL '1 day', tz AT TIME ZONE 'Europe/Paris', Age(dt, #2020-01-01#) FROM t",
    "CREATE TABLE u (k INTEGER PRIMARY KEY, v TEXT NOT NULL DEFAULT 'x', FOREIGN KEY (k) REFERENCES t (i) ON DELETE CASCADE DEFERRABLE)",
    "CREATE INDEX byS ON t (s); CREATE UNIQUE INDEX byI ON t (i)",
    "CREATE VIEW vv (a, b) AS SELECT i, s FROM t WHERE i > 0 WITH CHECK OPTION",
    "CREATE SEQUENCE sq START WITH 5 INCREMENT BY 2 MAXVALUE 100 CYCLE; SELECT NEXT VALUE FOR sq, NEXTVAL('sq'), CURRVAL('sq')",
    "CREATE TRIGGER tr AFTER INSERT OR UPDATE OF s ON t FOR EACH ROW WHEN NEW.i > 0 BEGIN INSERT INTO t (i) VALUES (NEW.i + 1); END",
    "CREATE FUNCTION f(@a INTEGER, @b INTEGER = 2) RETURNS INTEGER AS @a * @b; SELECT f(3), f(3, 4)",
    "CREATE PROCEDURE p(@n INTEGER) AS BEGIN DECLARE @i INTEGER = 0; WHILE @i < @n DO SET @i = @i + 1; END WHILE; RETURN @i; END; CALL p(3)",
    "INSERT INTO t (i, s) VALUES (7, 'seven') ON CONFLICT (i) DO UPDATE SET s = EXCLUDED.s RETURNING i, s",
    "UPDATE t SET i = i + 1, s = s & '!' WHERE i < 100 RETURNING *",
    "DELETE FROM t WHERE i IS NULL OR s = 'x'",
    "MERGE INTO t USING (SELECT 1 AS i) src ON t.i = src.i WHEN MATCHED THEN UPDATE SET s = 'm' WHEN NOT MATCHED THEN INSERT (i) VALUES (src.i)",
    "ALTER TABLE t ADD COLUMN z INTEGER DEFAULT 0; ALTER TABLE t RENAME COLUMN z TO zz; ALTER TABLE t DROP COLUMN zz",
    "BEGIN; INSERT INTO t (i) VALUES (99); SAVEPOINT a; DELETE FROM t; ROLLBACK TO a; COMMIT",
    "PREPARE q AS SELECT * FROM t WHERE i = @i; EXEC q (1); DEALLOCATE q",
    "SET TIME ZONE 'Asia/Tokyo'; SELECT tz FROM t",
    "SELECT * FROM information_schema.columns WHERE table_name = 't'",
    "EXPLAIN SELECT * FROM t WHERE i = 1",
    "CREATE REPORT rr TITLE 'R' BREAK BY b HEADER '{b}' COLUMN i SUM AS SELECT b, i FROM t; RUN REPORT rr AS markdown",
    "SELECT 'it''s', [odd name], #2024-03-05#, 1e3, 0.1, 123456789012345678901234567890, NULL, TRUE",
    "SELECT * FROM t WHERE (i, s) > (0, 'a') AND i BETWEEN SYMMETRIC 10 AND 0 AND s IN ('a', 'b') AND NOT d IS DISTINCT FROM 1.5",
    "SELECT CASE WHEN i > 1 THEN 'big' WHEN i = 1 THEN 'one' ELSE 'none' END, IIf(b, 1, 2), Nz(i, 0), Coalesce(s, 'n') FROM t",
    "SELECT DISTINCT TOP 3 s FROM t UNION SELECT s FROM t INTERSECT SELECT s FROM t EXCEPT SELECT 'z'",
    "SELECT Left(s, 2), Mid(s, 2, 1), InStr(s, 'n'), Replace(s, 'n', 'N'), Len(s), Trim('  x '), UCase(s) FROM t",
    "SELECT Sqr(2), Round(2.675, 2), Int(-1.5), Mod(7, 3), Power(2, 100), Atan2(1, 1), TRUNC(-1.55, 1), Exp(1) FROM t",
    "DROP TABLE IF EXISTS nothing; DROP VIEW IF EXISTS nothing; DROP INDEX IF EXISTS nothing ON t",
]


def seed(target: str, name: str, data: bytes) -> None:
    out = HERE / "seeds" / target
    out.mkdir(parents=True, exist_ok=True)
    (out / name).write_bytes(data)


for i, sql in enumerate(SQL):
    raw = sql.encode()
    seed("strings", f"exec-{i:02}", b"\x00" + raw)
    seed("strings", f"nts-{i:02}", b"\x01" + raw)
    seed("strings", f"prepare-{i:02}", b"\x02" + raw)
seed("strings", "report-text", b"\x03\x00r")
seed("strings", "report-pdf", b"\x03\x05r")
seed("strings", "report-bad", b"\x03\x02nosuch")
seed("strings", "param", b"\x04@Beta")
seed("strings", "param2", b"\x04gamma_3")
for op in (5, 6, 7):
    seed("strings", f"text{op}-plain", bytes([op]) + b"12.50")
    seed("strings", f"text{op}-unicode", bytes([op]) + "héllo 世界 \U0001F600".encode())
    seed("strings", f"text{op}-big", bytes([op]) + b"-123456789012345678901234567890")
    seed("strings", f"text{op}-bad", bytes([op]) + b"\xff\xfe\xc3")
seed("strings", "cut", bytes([8, 5]) + b"SELECT 'h\xc3\xa9llo' FROM t")
seed("strings", "open-memory", b"\x09:memory:")
seed("strings", "open-bad", b"\x09\xff\xfe")

# `binds` takes structured input; these just give it some bytes to start from.
for i in range(8):
    seed("binds", f"seed-{i}", struct.pack("<Q", 0x9E3779B97F4A7C15 * (i + 1) & 0xFFFFFFFFFFFFFFFF) * 16)
print("seeds written")
