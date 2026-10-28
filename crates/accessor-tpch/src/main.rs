//! TPC-H against Accessor: a generator in the shape of `dbgen` (the official tool is not part of the repository, so the
//! values differ from the official answers), the 22 queries with the parameters of the specification's validation run, and
//! three checks: every query runs, it finishes in time, and (with `--compare`) it gives the same rows with the optimizer
//! off. Usage: `accessor-tpch [--sf 0.01] [--seed N] [--compare] [--only 1,5,9] [--no-indexes] [--timeout SECONDS]`.
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use accessor_core::{csv::import_csv, Database};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// A number in `lo..=hi`.
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % (hi - lo + 1) as u64) as i64
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[(self.next() % items.len() as u64) as usize]
    }
}

/// Days since 1970-01-01 of a date, and back (Howard Hinnant's algorithms).
fn days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn date(z: i64) -> String {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

const NATIONS: [(&str, i64); 25] = [
    ("ALGERIA", 0), ("ARGENTINA", 1), ("BRAZIL", 1), ("CANADA", 1), ("EGYPT", 4), ("ETHIOPIA", 0), ("FRANCE", 3), ("GERMANY", 3),
    ("INDIA", 2), ("INDONESIA", 2), ("IRAN", 4), ("IRAQ", 4), ("JAPAN", 2), ("JORDAN", 4), ("KENYA", 0), ("MOROCCO", 0), ("MOZAMBIQUE", 0),
    ("PERU", 1), ("CHINA", 2), ("ROMANIA", 3), ("SAUDI ARABIA", 4), ("VIETNAM", 2), ("RUSSIA", 3), ("UNITED KINGDOM", 3), ("UNITED STATES", 1),
];
const REGIONS: [&str; 5] = ["AFRICA", "AMERICA", "ASIA", "EUROPE", "MIDDLE EAST"];
const COLORS: [&str; 30] = [
    "almond", "antique", "aquamarine", "azure", "beige", "bisque", "black", "blanched", "blue", "blush", "brown", "burlywood", "chartreuse",
    "chiffon", "chocolate", "coral", "cornflower", "cream", "cyan", "dark", "forest", "frosted", "gainsboro", "green", "honeydew", "ivory",
    "khaki", "lavender", "lime", "linen",
];
const TYPE1: [&str; 6] = ["STANDARD", "SMALL", "MEDIUM", "LARGE", "ECONOMY", "PROMO"];
const TYPE2: [&str; 5] = ["ANODIZED", "BURNISHED", "PLATED", "POLISHED", "BRUSHED"];
const TYPE3: [&str; 5] = ["TIN", "NICKEL", "BRASS", "STEEL", "COPPER"];
const CONT1: [&str; 5] = ["SM", "LG", "MED", "JUMBO", "WRAP"];
const CONT2: [&str; 8] = ["CASE", "BOX", "BAG", "JAR", "PKG", "PACK", "CAN", "DRUM"];
const SEGMENTS: [&str; 5] = ["AUTOMOBILE", "BUILDING", "FURNITURE", "MACHINERY", "HOUSEHOLD"];
const PRIORITIES: [&str; 5] = ["1-URGENT", "2-HIGH", "3-MEDIUM", "4-NOT SPECIFIED", "5-LOW"];
const MODES: [&str; 7] = ["REG AIR", "AIR", "RAIL", "SHIP", "TRUCK", "MAIL", "FOB"];
const INSTRUCT: [&str; 4] = ["DELIVER IN PERSON", "COLLECT COD", "NONE", "TAKE BACK RETURN"];
const WORDS: [&str; 12] = ["furiously", "carefully", "blithely", "quickly", "slyly", "boldly", "final", "ironic", "pending", "regular", "express", "bold"];

fn comment(r: &mut Rng) -> String {
    format!("{} {} {}", r.pick(&WORDS), r.pick(&WORDS), r.pick(&WORDS))
}

fn money(cents: i64) -> String {
    format!("{}{}.{:02}", if cents < 0 { "-" } else { "" }, cents.abs() / 100, cents.abs() % 100)
}

struct Sizes {
    supplier: i64,
    part: i64,
    customer: i64,
    orders: i64,
}

const SCHEMA: &str = "
CREATE TABLE region (r_regionkey INTEGER PRIMARY KEY, r_name TEXT NOT NULL, r_comment TEXT);
CREATE TABLE nation (n_nationkey INTEGER PRIMARY KEY, n_name TEXT NOT NULL, n_regionkey INTEGER NOT NULL REFERENCES region, n_comment TEXT);
CREATE TABLE part (p_partkey INTEGER PRIMARY KEY, p_name TEXT NOT NULL, p_mfgr TEXT NOT NULL, p_brand TEXT NOT NULL, p_type TEXT NOT NULL,
  p_size INTEGER NOT NULL, p_container TEXT NOT NULL, p_retailprice DECIMAL(15,2) NOT NULL, p_comment TEXT);
CREATE TABLE supplier (s_suppkey INTEGER PRIMARY KEY, s_name TEXT NOT NULL, s_address TEXT, s_nationkey INTEGER NOT NULL REFERENCES nation,
  s_phone TEXT, s_acctbal DECIMAL(15,2) NOT NULL, s_comment TEXT);
CREATE TABLE partsupp (ps_partkey INTEGER NOT NULL REFERENCES part, ps_suppkey INTEGER NOT NULL REFERENCES supplier, ps_availqty INTEGER NOT NULL,
  ps_supplycost DECIMAL(15,2) NOT NULL, ps_comment TEXT, PRIMARY KEY (ps_partkey, ps_suppkey));
CREATE TABLE customer (c_custkey INTEGER PRIMARY KEY, c_name TEXT NOT NULL, c_address TEXT, c_nationkey INTEGER NOT NULL REFERENCES nation,
  c_phone TEXT, c_acctbal DECIMAL(15,2) NOT NULL, c_mktsegment TEXT, c_comment TEXT);
CREATE TABLE orders (o_orderkey INTEGER PRIMARY KEY, o_custkey INTEGER NOT NULL REFERENCES customer, o_orderstatus TEXT NOT NULL,
  o_totalprice DECIMAL(15,2) NOT NULL, o_orderdate DATE NOT NULL, o_orderpriority TEXT, o_clerk TEXT, o_shippriority INTEGER, o_comment TEXT);
CREATE TABLE lineitem (l_orderkey INTEGER NOT NULL REFERENCES orders, l_partkey INTEGER NOT NULL, l_suppkey INTEGER NOT NULL, l_linenumber INTEGER NOT NULL,
  l_quantity DECIMAL(15,2) NOT NULL, l_extendedprice DECIMAL(15,2) NOT NULL, l_discount DECIMAL(15,2) NOT NULL, l_tax DECIMAL(15,2) NOT NULL,
  l_returnflag TEXT NOT NULL, l_linestatus TEXT NOT NULL, l_shipdate DATE NOT NULL, l_commitdate DATE NOT NULL, l_receiptdate DATE NOT NULL,
  l_shipinstruct TEXT, l_shipmode TEXT, l_comment TEXT, PRIMARY KEY (l_orderkey, l_linenumber),
  FOREIGN KEY (l_partkey, l_suppkey) REFERENCES partsupp (ps_partkey, ps_suppkey));";

const INDEXES: &str = "
CREATE INDEX n_regionkey ON nation (n_regionkey);
CREATE INDEX s_nationkey ON supplier (s_nationkey);
CREATE INDEX c_nationkey ON customer (c_nationkey);
CREATE INDEX ps_suppkey ON partsupp (ps_suppkey);
CREATE INDEX o_custkey ON orders (o_custkey);
CREATE INDEX o_orderdate ON orders (o_orderdate);
CREATE INDEX l_partkey ON lineitem (l_partkey, l_suppkey);
CREATE INDEX l_shipdate ON lineitem (l_shipdate);
CREATE INDEX l_receiptdate ON lineitem (l_receiptdate);";

fn load(db: &mut Database, sf: f64, seed: u64) -> Sizes {
    let mut r = Rng(seed | 1);
    let sz = Sizes {
        supplier: ((10_000.0 * sf) as i64).max(4),
        part: ((200_000.0 * sf) as i64).max(20),
        customer: ((150_000.0 * sf) as i64).max(30),
        orders: ((1_500_000.0 * sf) as i64).max(300),
    };
    let mut put = |db: &mut Database, table: &str, csv: String| {
        import_csv(db, table, &csv, false).unwrap_or_else(|e| panic!("loading {table}: {e}"));
    };
    let mut s = String::new();
    for (i, n) in REGIONS.iter().enumerate() {
        let _ = writeln!(s, "{i},{n},{}", comment(&mut r));
    }
    put(db, "region", std::mem::take(&mut s));
    for (i, (n, reg)) in NATIONS.iter().enumerate() {
        let _ = writeln!(s, "{i},{n},{reg},{}", comment(&mut r));
    }
    put(db, "nation", std::mem::take(&mut s));
    let mut retail = Vec::new();
    for k in 1..=sz.part {
        let name: Vec<&str> = (0..5).map(|_| r.pick(&COLORS)).collect();
        let m = r.range(1, 5);
        let price = (90_000 + ((k / 10) % 20_001) + 100 * (k % 1000)) as i64;
        retail.push(price);
        let _ = writeln!(
            s,
            "{k},{},Manufacturer#{m},Brand#{m}{},{} {} {},{},{} {},{},{}",
            name.join(" "),
            r.range(1, 5),
            r.pick(&TYPE1),
            r.pick(&TYPE2),
            r.pick(&TYPE3),
            r.range(1, 50),
            r.pick(&CONT1),
            r.pick(&CONT2),
            money(price),
            comment(&mut r)
        );
    }
    put(db, "part", std::mem::take(&mut s));
    for k in 1..=sz.supplier {
        let nk = r.range(0, 24);
        let c = if r.range(1, 100) == 1 { "Customer Complaints noted".to_string() } else { comment(&mut r) };
        let _ = writeln!(s, "{k},Supplier#{k:09},addr{k},{nk},{}-{}-{}-{},{},{c}", nk + 10, r.range(100, 999), r.range(100, 999), r.range(1000, 9999), money(r.range(-99_999, 999_999)));
    }
    put(db, "supplier", std::mem::take(&mut s));
    for p in 1..=sz.part {
        for i in 0..4 {
            let supp = (p + i * (sz.supplier / 4 + (p - 1) / sz.supplier)) % sz.supplier + 1;
            let _ = writeln!(s, "{p},{supp},{},{},{}", r.range(1, 9999), money(r.range(100, 100_000)), comment(&mut r));
        }
    }
    put(db, "partsupp", std::mem::take(&mut s));
    for k in 1..=sz.customer {
        let nk = r.range(0, 24);
        let _ = writeln!(s, "{k},Customer#{k:09},addr{k},{nk},{}-{}-{}-{},{},{},{}", nk + 10, r.range(100, 999), r.range(100, 999), r.range(1000, 9999), money(r.range(-99_999, 999_999)), r.pick(&SEGMENTS), comment(&mut r));
    }
    put(db, "customer", std::mem::take(&mut s));
    let (first, last) = (days(1992, 1, 1), days(1998, 8, 2));
    let current = days(1995, 6, 17);
    let mut lines = String::new();
    for i in 1..=sz.orders {
        // the order keys leave gaps, as in dbgen: 8 used of every 32
        let key = (i - 1) / 8 * 32 + (i - 1) % 8 + 1;
        let mut cust = r.range(1, sz.customer);
        while cust % 3 == 0 {
            cust = r.range(1, sz.customer);
        }
        let od = r.range(first, last);
        let (mut total, mut f, mut o) = (0i64, 0, 0);
        let n = r.range(1, 7);
        for ln in 1..=n {
            let p = r.range(1, sz.part);
            let supp = (p + r.range(0, 3) * (sz.supplier / 4 + (p - 1) / sz.supplier)) % sz.supplier + 1;
            let qty = r.range(1, 50);
            let ext = qty * retail[(p - 1) as usize];
            let disc = r.range(0, 10);
            let tax = r.range(0, 8);
            let ship = od + r.range(1, 121);
            let commit = od + r.range(30, 90);
            let receipt = ship + r.range(1, 30);
            let flag = if receipt <= current { if r.range(0, 1) == 0 { "R" } else { "A" } } else { "N" };
            let status = if ship <= current { f += 1; "F" } else { o += 1; "O" };
            total += ext * (100 + tax) / 100 * (100 - disc) / 100;
            let _ = writeln!(
                lines,
                "{key},{p},{supp},{ln},{qty}.00,{},0.{disc:02},0.{tax:02},{flag},{status},{},{},{},{},{},{}",
                money(ext),
                date(ship),
                date(commit),
                date(receipt),
                r.pick(&INSTRUCT),
                r.pick(&MODES),
                comment(&mut r)
            );
        }
        let status = if o == 0 { "F" } else if f == 0 { "O" } else { "P" };
        let c = if r.range(1, 50) == 1 { "special pending requests".to_string() } else { comment(&mut r) };
        let _ = writeln!(s, "{key},{cust},{status},{},{},{},Clerk#{:09},0,{c}", money(total), date(od), r.pick(&PRIORITIES), r.range(1, (sz.orders / 1500).max(1) * 1000));
    }
    put(db, "orders", std::mem::take(&mut s));
    put(db, "lineitem", lines);
    sz
}

/// The 22 queries with the parameters of the specification's validation run.
fn queries(sf: f64) -> Vec<(u32, String)> {
    let fraction = 0.0001 / sf;
    let q = |n: u32, s: &str| (n, s.to_string());
    vec![
        q(1, "SELECT l_returnflag, l_linestatus, SUM(l_quantity) AS sum_qty, SUM(l_extendedprice) AS sum_base_price,
  SUM(l_extendedprice * (1 - l_discount)) AS sum_disc_price, SUM(l_extendedprice * (1 - l_discount) * (1 + l_tax)) AS sum_charge,
  AVG(l_quantity) AS avg_qty, AVG(l_extendedprice) AS avg_price, AVG(l_discount) AS avg_disc, COUNT(*) AS count_order
FROM lineitem WHERE l_shipdate <= DATE '1998-09-02' GROUP BY l_returnflag, l_linestatus ORDER BY l_returnflag, l_linestatus"),
        q(2, "SELECT s_acctbal, s_name, n_name, p_partkey, p_mfgr, s_address, s_phone, s_comment
FROM part, supplier, partsupp, nation, region
WHERE p_partkey = ps_partkey AND s_suppkey = ps_suppkey AND p_size = 15 AND p_type LIKE '%BRASS' AND s_nationkey = n_nationkey
  AND n_regionkey = r_regionkey AND r_name = 'EUROPE'
  AND ps_supplycost = (SELECT MIN(ps_supplycost) FROM partsupp, supplier, nation, region
    WHERE p_partkey = ps_partkey AND s_suppkey = ps_suppkey AND s_nationkey = n_nationkey AND n_regionkey = r_regionkey AND r_name = 'EUROPE')
ORDER BY s_acctbal DESC, n_name, s_name, p_partkey LIMIT 100"),
        q(3, "SELECT l_orderkey, SUM(l_extendedprice * (1 - l_discount)) AS revenue, o_orderdate, o_shippriority
FROM customer, orders, lineitem
WHERE c_mktsegment = 'BUILDING' AND c_custkey = o_custkey AND l_orderkey = o_orderkey AND o_orderdate < DATE '1995-03-15' AND l_shipdate > DATE '1995-03-15'
GROUP BY l_orderkey, o_orderdate, o_shippriority ORDER BY revenue DESC, o_orderdate LIMIT 10"),
        q(4, "SELECT o_orderpriority, COUNT(*) AS order_count FROM orders
WHERE o_orderdate >= DATE '1993-07-01' AND o_orderdate < DATE '1993-10-01'
  AND EXISTS (SELECT * FROM lineitem WHERE l_orderkey = o_orderkey AND l_commitdate < l_receiptdate)
GROUP BY o_orderpriority ORDER BY o_orderpriority"),
        q(5, "SELECT n_name, SUM(l_extendedprice * (1 - l_discount)) AS revenue
FROM customer, orders, lineitem, supplier, nation, region
WHERE c_custkey = o_custkey AND l_orderkey = o_orderkey AND l_suppkey = s_suppkey AND c_nationkey = s_nationkey AND s_nationkey = n_nationkey
  AND n_regionkey = r_regionkey AND r_name = 'ASIA' AND o_orderdate >= DATE '1994-01-01' AND o_orderdate < DATE '1995-01-01'
GROUP BY n_name ORDER BY revenue DESC"),
        q(6, "SELECT SUM(l_extendedprice * l_discount) AS revenue FROM lineitem
WHERE l_shipdate >= DATE '1994-01-01' AND l_shipdate < DATE '1995-01-01' AND l_discount BETWEEN 0.05 AND 0.07 AND l_quantity < 24"),
        q(7, "SELECT supp_nation, cust_nation, l_year, SUM(volume) AS revenue FROM (
  SELECT n1.n_name AS supp_nation, n2.n_name AS cust_nation, EXTRACT(YEAR FROM l_shipdate) AS l_year, l_extendedprice * (1 - l_discount) AS volume
  FROM supplier, lineitem, orders, customer, nation n1, nation n2
  WHERE s_suppkey = l_suppkey AND o_orderkey = l_orderkey AND c_custkey = o_custkey AND s_nationkey = n1.n_nationkey AND c_nationkey = n2.n_nationkey
    AND ((n1.n_name = 'FRANCE' AND n2.n_name = 'GERMANY') OR (n1.n_name = 'GERMANY' AND n2.n_name = 'FRANCE'))
    AND l_shipdate BETWEEN DATE '1995-01-01' AND DATE '1996-12-31') AS shipping
GROUP BY supp_nation, cust_nation, l_year ORDER BY supp_nation, cust_nation, l_year"),
        q(8, "SELECT o_year, SUM(CASE WHEN nation = 'BRAZIL' THEN volume ELSE 0 END) / SUM(volume) AS mkt_share FROM (
  SELECT EXTRACT(YEAR FROM o_orderdate) AS o_year, l_extendedprice * (1 - l_discount) AS volume, n2.n_name AS nation
  FROM part, supplier, lineitem, orders, customer, nation n1, nation n2, region
  WHERE p_partkey = l_partkey AND s_suppkey = l_suppkey AND l_orderkey = o_orderkey AND o_custkey = c_custkey AND c_nationkey = n1.n_nationkey
    AND n1.n_regionkey = r_regionkey AND r_name = 'AMERICA' AND s_nationkey = n2.n_nationkey
    AND o_orderdate BETWEEN DATE '1995-01-01' AND DATE '1996-12-31' AND p_type = 'ECONOMY ANODIZED STEEL') AS all_nations
GROUP BY o_year ORDER BY o_year"),
        q(9, "SELECT nation, o_year, SUM(amount) AS sum_profit FROM (
  SELECT n_name AS nation, EXTRACT(YEAR FROM o_orderdate) AS o_year, l_extendedprice * (1 - l_discount) - ps_supplycost * l_quantity AS amount
  FROM part, supplier, lineitem, partsupp, orders, nation
  WHERE s_suppkey = l_suppkey AND ps_suppkey = l_suppkey AND ps_partkey = l_partkey AND p_partkey = l_partkey AND o_orderkey = l_orderkey
    AND s_nationkey = n_nationkey AND p_name LIKE '%green%') AS profit
GROUP BY nation, o_year ORDER BY nation, o_year DESC"),
        q(10, "SELECT c_custkey, c_name, SUM(l_extendedprice * (1 - l_discount)) AS revenue, c_acctbal, n_name, c_address, c_phone, c_comment
FROM customer, orders, lineitem, nation
WHERE c_custkey = o_custkey AND l_orderkey = o_orderkey AND o_orderdate >= DATE '1993-10-01' AND o_orderdate < DATE '1994-01-01'
  AND l_returnflag = 'R' AND c_nationkey = n_nationkey
GROUP BY c_custkey, c_name, c_acctbal, c_phone, n_name, c_address, c_comment ORDER BY revenue DESC, c_custkey LIMIT 20"),
        (11, format!("SELECT ps_partkey, SUM(ps_supplycost * ps_availqty) AS value FROM partsupp, supplier, nation
WHERE ps_suppkey = s_suppkey AND s_nationkey = n_nationkey AND n_name = 'GERMANY'
GROUP BY ps_partkey HAVING SUM(ps_supplycost * ps_availqty) > (SELECT SUM(ps_supplycost * ps_availqty) * {fraction} FROM partsupp, supplier, nation
  WHERE ps_suppkey = s_suppkey AND s_nationkey = n_nationkey AND n_name = 'GERMANY')
ORDER BY value DESC, ps_partkey")),
        q(12, "SELECT l_shipmode, SUM(CASE WHEN o_orderpriority = '1-URGENT' OR o_orderpriority = '2-HIGH' THEN 1 ELSE 0 END) AS high_line_count,
  SUM(CASE WHEN o_orderpriority <> '1-URGENT' AND o_orderpriority <> '2-HIGH' THEN 1 ELSE 0 END) AS low_line_count
FROM orders, lineitem WHERE o_orderkey = l_orderkey AND l_shipmode IN ('MAIL', 'SHIP') AND l_commitdate < l_receiptdate AND l_shipdate < l_commitdate
  AND l_receiptdate >= DATE '1994-01-01' AND l_receiptdate < DATE '1995-01-01'
GROUP BY l_shipmode ORDER BY l_shipmode"),
        q(13, "SELECT c_count, COUNT(*) AS custdist FROM (
  SELECT c_custkey, COUNT(o_orderkey) AS c_count FROM customer LEFT OUTER JOIN orders ON c_custkey = o_custkey AND o_comment NOT LIKE '%special%requests%'
  GROUP BY c_custkey) AS c_orders GROUP BY c_count ORDER BY custdist DESC, c_count DESC"),
        q(14, "SELECT 100.00 * SUM(CASE WHEN p_type LIKE 'PROMO%' THEN l_extendedprice * (1 - l_discount) ELSE 0 END) / SUM(l_extendedprice * (1 - l_discount)) AS promo_revenue
FROM lineitem, part WHERE l_partkey = p_partkey AND l_shipdate >= DATE '1995-09-01' AND l_shipdate < DATE '1995-10-01'"),
        q(15, "WITH revenue0 AS (SELECT l_suppkey AS supplier_no, SUM(l_extendedprice * (1 - l_discount)) AS total_revenue FROM lineitem
  WHERE l_shipdate >= DATE '1996-01-01' AND l_shipdate < DATE '1996-04-01' GROUP BY l_suppkey)
SELECT s_suppkey, s_name, s_address, s_phone, total_revenue FROM supplier, revenue0
WHERE s_suppkey = supplier_no AND total_revenue = (SELECT MAX(total_revenue) FROM revenue0) ORDER BY s_suppkey"),
        q(16, "SELECT p_brand, p_type, p_size, COUNT(DISTINCT ps_suppkey) AS supplier_cnt FROM partsupp, part
WHERE p_partkey = ps_partkey AND p_brand <> 'Brand#45' AND p_type NOT LIKE 'MEDIUM POLISHED%' AND p_size IN (49, 14, 23, 45, 19, 3, 36, 9)
  AND ps_suppkey NOT IN (SELECT s_suppkey FROM supplier WHERE s_comment LIKE '%Customer%Complaints%')
GROUP BY p_brand, p_type, p_size ORDER BY supplier_cnt DESC, p_brand, p_type, p_size"),
        q(17, "SELECT SUM(l_extendedprice) / 7.0 AS avg_yearly FROM lineitem, part
WHERE p_partkey = l_partkey AND p_brand = 'Brand#23' AND p_container = 'MED BOX'
  AND l_quantity < (SELECT 0.2 * AVG(l_quantity) FROM lineitem WHERE l_partkey = p_partkey)"),
        q(18, "SELECT c_name, c_custkey, o_orderkey, o_orderdate, o_totalprice, SUM(l_quantity) FROM customer, orders, lineitem
WHERE o_orderkey IN (SELECT l_orderkey FROM lineitem GROUP BY l_orderkey HAVING SUM(l_quantity) > 300)
  AND c_custkey = o_custkey AND o_orderkey = l_orderkey
GROUP BY c_name, c_custkey, o_orderkey, o_orderdate, o_totalprice ORDER BY o_totalprice DESC, o_orderdate LIMIT 100"),
        q(19, "SELECT SUM(l_extendedprice * (1 - l_discount)) AS revenue FROM lineitem, part WHERE
  (p_partkey = l_partkey AND p_brand = 'Brand#12' AND p_container IN ('SM CASE', 'SM BOX', 'SM PACK', 'SM PKG') AND l_quantity >= 1 AND l_quantity <= 11
    AND p_size BETWEEN 1 AND 5 AND l_shipmode IN ('AIR', 'AIR REG') AND l_shipinstruct = 'DELIVER IN PERSON')
  OR (p_partkey = l_partkey AND p_brand = 'Brand#23' AND p_container IN ('MED BAG', 'MED BOX', 'MED PKG', 'MED PACK') AND l_quantity >= 10 AND l_quantity <= 20
    AND p_size BETWEEN 1 AND 10 AND l_shipmode IN ('AIR', 'AIR REG') AND l_shipinstruct = 'DELIVER IN PERSON')
  OR (p_partkey = l_partkey AND p_brand = 'Brand#34' AND p_container IN ('LG CASE', 'LG BOX', 'LG PACK', 'LG PKG') AND l_quantity >= 20 AND l_quantity <= 30
    AND p_size BETWEEN 1 AND 15 AND l_shipmode IN ('AIR', 'AIR REG') AND l_shipinstruct = 'DELIVER IN PERSON')"),
        q(20, "SELECT s_name, s_address FROM supplier, nation
WHERE s_suppkey IN (SELECT ps_suppkey FROM partsupp WHERE ps_partkey IN (SELECT p_partkey FROM part WHERE p_name LIKE 'forest%')
    AND ps_availqty > (SELECT 0.5 * SUM(l_quantity) FROM lineitem WHERE l_partkey = ps_partkey AND l_suppkey = ps_suppkey
      AND l_shipdate >= DATE '1994-01-01' AND l_shipdate < DATE '1995-01-01'))
  AND s_nationkey = n_nationkey AND n_name = 'CANADA' ORDER BY s_name"),
        q(21, "SELECT s_name, COUNT(*) AS numwait FROM supplier, lineitem l1, orders, nation
WHERE s_suppkey = l1.l_suppkey AND o_orderkey = l1.l_orderkey AND o_orderstatus = 'F' AND l1.l_receiptdate > l1.l_commitdate
  AND EXISTS (SELECT * FROM lineitem l2 WHERE l2.l_orderkey = l1.l_orderkey AND l2.l_suppkey <> l1.l_suppkey)
  AND NOT EXISTS (SELECT * FROM lineitem l3 WHERE l3.l_orderkey = l1.l_orderkey AND l3.l_suppkey <> l1.l_suppkey AND l3.l_receiptdate > l3.l_commitdate)
  AND s_nationkey = n_nationkey AND n_name = 'SAUDI ARABIA' GROUP BY s_name ORDER BY numwait DESC, s_name LIMIT 100"),
        q(22, "SELECT cntrycode, COUNT(*) AS numcust, SUM(c_acctbal) AS totacctbal FROM (
  SELECT SUBSTRING(c_phone FROM 1 FOR 2) AS cntrycode, c_acctbal FROM customer
  WHERE SUBSTRING(c_phone FROM 1 FOR 2) IN ('13', '31', '23', '29', '30', '18', '17')
    AND c_acctbal > (SELECT AVG(c_acctbal) FROM customer WHERE c_acctbal > 0.00 AND SUBSTRING(c_phone FROM 1 FOR 2) IN ('13', '31', '23', '29', '30', '18', '17'))
    AND NOT EXISTS (SELECT * FROM orders WHERE o_custkey = c_custkey)) AS custsale
GROUP BY cntrycode ORDER BY cntrycode"),
    ]
}

fn run(db: &mut Database, sql: &str) -> Result<(Vec<String>, Duration), String> {
    let t = Instant::now();
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| db.execute(sql)));
    match res {
        Err(_) => Err("PANIC".into()),
        Ok(Err(e)) => Err(e.to_string()),
        Ok(Ok(r)) => Ok((r.rows.iter().map(|row| row.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("|")).collect(), t.elapsed())),
    }
}

fn main() {
    let mut explain = false;
    let mut extra: Vec<String> = Vec::new();
    let (mut sf, mut seed, mut compare, mut only, mut indexes, mut timeout) = (0.01f64, 1u64, false, Vec::<u32>::new(), true, 60u64);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--sf" => sf = it.next().and_then(|v| v.parse().ok()).expect("--sf N"),
            "--seed" => seed = it.next().and_then(|v| v.parse().ok()).expect("--seed N"),
            "--compare" => compare = true,
            "--no-indexes" => indexes = false,
            "--timeout" => timeout = it.next().and_then(|v| v.parse().ok()).expect("--timeout SECONDS"),
            "--explain" => explain = true,
            "--sql" => extra.push(it.next().expect("--sql QUERY").clone()),
            "--only" => only = it.next().expect("--only 1,2").split(',').filter_map(|v| v.parse().ok()).collect(),
            other => panic!("unknown argument {other}"),
        }
    }
    let mut db = Database::in_memory();
    let started = Instant::now();
    db.execute(SCHEMA).expect("the schema");
    let sizes = load(&mut db, sf, seed);
    if indexes {
        db.execute(INDEXES).expect("the indexes");
    }
    let lines = db.execute("SELECT COUNT(*) FROM lineitem").map(|r| r.rows[0][0].to_string()).unwrap_or_default();
    println!(
        "scale factor {sf}: {} suppliers, {} parts, {} customers, {} orders, {lines} line items, loaded in {:.1}s{}",
        sizes.supplier,
        sizes.part,
        sizes.customer,
        sizes.orders,
        started.elapsed().as_secs_f64(),
        if indexes { "" } else { " (no indexes)" }
    );
    let _ = db.execute(&format!("SET statement_timeout = '{}s'", timeout));
    // the same data without indexes: other access paths must give the same rows
    let mut twin = Database::in_memory();
    if compare && indexes {
        twin.execute(SCHEMA).expect("the schema");
        load(&mut twin, sf, seed);
        let _ = twin.execute(&format!("SET statement_timeout = '{}s'", timeout));
    }
    std::panic::set_hook(Box::new(|_| {}));
    let mut bad = 0;
    for sql in &extra {
        match run(&mut db, sql) {
            Ok((rows, took)) => println!("{:.3}s, {} rows, first: {}", took.as_secs_f64(), rows.len(), rows.first().cloned().unwrap_or_default()),
            Err(e) => println!("FAILED: {e}"),
        }
    }
    if !extra.is_empty() {
        return;
    }
    println!("{:>3} {:>7} {:>9}  {}", "Q", "rows", "seconds", "result");
    for (n, sql) in queries(sf) {
        if !only.is_empty() && !only.contains(&n) {
            continue;
        }
        if explain {
            match run(&mut db, &format!("EXPLAIN {sql}")) {
                Ok((plan, _)) => println!("{n}:
{}", plan.join("
")),
                Err(e) => println!("{n}: EXPLAIN failed: {e}"),
            }
            continue;
        }
        match run(&mut db, &sql) {
            Err(e) => {
                bad += 1;
                println!("{n:>3} {:>7} {:>9}  FAILED: {e}", "", "");
            }
            Ok((rows, took)) => {
                let mut verdict = "ok".to_string();
                if compare {
                    let mut mine = rows.clone();
                    mine.sort();
                    let mut checked = Vec::new();
                    let mut variants: Vec<(&str, bool)> = vec![("optimizer off", false)];
                    if indexes {
                        variants.push(("no indexes", true));
                    }
                    for (what, use_twin) in variants {
                        let d = if use_twin { &mut twin } else { &mut db };
                        d.set_optimizer(use_twin);
                        let other = run(d, &sql);
                        d.set_optimizer(true);
                        match other {
                            Err(e) => checked.push(format!("{what}: {}", if e.contains("timeout") { "too slow".to_string() } else { e })),
                            Ok((mut other, _)) => {
                                other.sort();
                                if mine != other {
                                    bad += 1;
                                    checked.push(format!("DIFFERENT with {what} ({} rows against {})", other.len(), rows.len()));
                                } else {
                                    checked.push(format!("same with {what}"));
                                }
                            }
                        }
                    }
                    verdict = checked.join("; ");
                }
                println!("{n:>3} {:>7} {:>9.3}  {verdict}", rows.len(), took.as_secs_f64());
            }
        }
    }
    if bad > 0 {
        std::process::exit(1);
    }
}
