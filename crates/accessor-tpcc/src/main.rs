//! TPC-C against Accessor: the nine tables loaded in the shape the specification gives, the five transactions run from
//! several terminals at once (each a connection on its own thread, in the mix of the specification: 45% New-Order, 43%
//! Payment, 4% each Order-Status, Delivery and Stock-Level), and then the consistency conditions of the specification
//! (clause 3.3.2) and two totals kept by the terminals: the orders and the payments that were committed must be the
//! ones the tables show. A transaction that is told to roll back and start over (`Error::Conflict`) is run again, and
//! counted. There is no network and no think time, so the figures are a test of correctness, not a TPC result.
//!
//! Usage: `accessor-tpcc [--warehouses 2] [--terminals 8] [--seconds 10] [--items 10000] [--customers 300] [--seed N]
//! [--no-lock] [--no-fk]`. `--customers` is the number per district (3000 in the specification); `--items` is 100000 there.
//! `--no-lock` leaves out `FOR UPDATE`, so the transactions meet on the rows they change and are told to retry.
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use accessor_core::{csv::import_csv, Connection, Database, Error, SharedDatabase, Value};

const DISTRICTS: i64 = 10;

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
    /// The non-uniform random of clause 2.1.6.
    fn nurand(&mut self, a: i64, c: i64, lo: i64, hi: i64) -> i64 {
        (((self.range(0, a) | self.range(lo, hi)) + c) % (hi - lo + 1)) + lo
    }
    fn alpha(&mut self, lo: usize, hi: usize) -> String {
        let n = self.range(lo as i64, hi as i64) as usize;
        (0..n).map(|_| (b'a' + (self.next() % 26) as u8) as char).collect()
    }
    fn digits(&mut self, n: usize) -> String {
        (0..n).map(|_| (b'0' + (self.next() % 10) as u8) as char).collect()
    }
    /// Text of `lo..=hi` characters; one time in ten it contains `ORIGINAL`.
    fn data(&mut self, lo: usize, hi: usize) -> String {
        let mut s = self.alpha(lo, hi);
        if self.range(1, 10) == 1 && s.len() >= 8 {
            let at = self.range(0, (s.len() - 8) as i64) as usize;
            s.replace_range(at..at + 8, "ORIGINAL");
        }
        s
    }
}

struct Cfg {
    warehouses: i64,
    items: i64,
    customers: i64,
    lock: bool,
    /// The constants of `nurand`, chosen once per run.
    c_last: i64,
    c_id: i64,
    c_item: i64,
}

impl Cfg {
    /// The number of last names there are: 1000 in the specification.
    fn names(&self) -> i64 {
        self.customers.min(1000)
    }
    /// Orders there are for each district when the run starts, and how many of the last of them are undelivered.
    fn orders(&self) -> i64 {
        self.customers
    }
    fn undelivered(&self) -> i64 {
        self.customers * 3 / 10
    }
    fn lock_clause(&self) -> &'static str {
        if self.lock {
            " FOR UPDATE"
        } else {
            ""
        }
    }
    fn pick_customer(&self, r: &mut Rng) -> i64 {
        r.nurand(pow2_below(self.customers / 3), self.c_id, 1, self.customers)
    }
    fn pick_item(&self, r: &mut Rng) -> i64 {
        r.nurand(pow2_below(self.items / 12), self.c_item, 1, self.items)
    }
    fn pick_last(&self, r: &mut Rng) -> String {
        last_name(r.nurand(255.min(pow2_below(self.names())), self.c_last, 0, self.names() - 1))
    }
}

/// The greatest `2^k - 1` that is at most `n` (at least 1).
fn pow2_below(n: i64) -> i64 {
    let mut a = 1;
    while a * 2 + 1 <= n {
        a = a * 2 + 1;
    }
    a
}

fn last_name(n: i64) -> String {
    const SYL: [&str; 10] = ["BAR", "OUGHT", "ABLE", "PRI", "PRES", "ESE", "ANTI", "CALLY", "ATION", "EING"];
    format!("{}{}{}", SYL[(n / 100 % 10) as usize], SYL[(n / 10 % 10) as usize], SYL[(n % 10) as usize])
}

fn money(cents: i64) -> String {
    format!("{}{}.{:02}", if cents < 0 { "-" } else { "" }, cents.abs() / 100, cents.abs() % 100)
}

/// A number of cents from a `DECIMAL` (or integer) value.
fn cents(v: &Value) -> i64 {
    let s = v.to_string();
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest.to_string()),
        None => (false, s),
    };
    let (whole, frac) = s.split_once('.').unwrap_or((&s, ""));
    let frac = format!("{frac:0<2}");
    let n = whole.parse::<i64>().unwrap_or(0) * 100 + frac[..2].parse::<i64>().unwrap_or(0);
    if neg {
        -n
    } else {
        n
    }
}

fn int(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i,
        other => other.to_string().parse().unwrap_or_else(|_| panic!("not an integer: {other:?}")),
    }
}

fn text(v: &Value) -> String {
    v.to_string()
}

fn schema(fk: bool) -> String {
    let r = |s: &str| if fk { format!(" REFERENCES {s}") } else { String::new() };
    let mut s = String::new();
    let _ = write!(
        s,
        "
CREATE TABLE warehouse (w_id INTEGER PRIMARY KEY, w_name TEXT, w_street_1 TEXT, w_street_2 TEXT, w_city TEXT, w_state TEXT, w_zip TEXT,
  w_tax DECIMAL(4,4) NOT NULL, w_ytd DECIMAL(12,2) NOT NULL);
CREATE TABLE district (d_id INTEGER NOT NULL, d_w_id INTEGER NOT NULL{w}, d_name TEXT, d_street_1 TEXT, d_street_2 TEXT, d_city TEXT, d_state TEXT,
  d_zip TEXT, d_tax DECIMAL(4,4) NOT NULL, d_ytd DECIMAL(12,2) NOT NULL, d_next_o_id INTEGER NOT NULL, PRIMARY KEY (d_w_id, d_id));
CREATE TABLE customer (c_id INTEGER NOT NULL, c_d_id INTEGER NOT NULL, c_w_id INTEGER NOT NULL, c_first TEXT, c_middle TEXT, c_last TEXT,
  c_street_1 TEXT, c_street_2 TEXT, c_city TEXT, c_state TEXT, c_zip TEXT, c_phone TEXT, c_since DATETIME, c_credit TEXT, c_credit_lim DECIMAL(12,2),
  c_discount DECIMAL(4,4) NOT NULL, c_balance DECIMAL(12,2) NOT NULL, c_ytd_payment DECIMAL(12,2) NOT NULL, c_payment_cnt INTEGER NOT NULL,
  c_delivery_cnt INTEGER NOT NULL, c_data TEXT, PRIMARY KEY (c_w_id, c_d_id, c_id){cfk});
CREATE TABLE history (h_c_id INTEGER, h_c_d_id INTEGER, h_c_w_id INTEGER, h_d_id INTEGER, h_w_id INTEGER, h_date DATETIME,
  h_amount DECIMAL(6,2) NOT NULL, h_data TEXT{hfk});
CREATE TABLE orders (o_id INTEGER NOT NULL, o_d_id INTEGER NOT NULL, o_w_id INTEGER NOT NULL, o_c_id INTEGER NOT NULL, o_entry_d DATETIME,
  o_carrier_id INTEGER, o_ol_cnt INTEGER NOT NULL, o_all_local INTEGER NOT NULL, PRIMARY KEY (o_w_id, o_d_id, o_id){ofk});
CREATE TABLE new_order (no_o_id INTEGER NOT NULL, no_d_id INTEGER NOT NULL, no_w_id INTEGER NOT NULL, PRIMARY KEY (no_w_id, no_d_id, no_o_id){nfk});
CREATE TABLE item (i_id INTEGER PRIMARY KEY, i_im_id INTEGER, i_name TEXT, i_price DECIMAL(5,2) NOT NULL, i_data TEXT);
CREATE TABLE stock (s_i_id INTEGER NOT NULL{si}, s_w_id INTEGER NOT NULL{sw}, s_quantity INTEGER NOT NULL, s_dist_01 TEXT, s_dist_02 TEXT,
  s_dist_03 TEXT, s_dist_04 TEXT, s_dist_05 TEXT, s_dist_06 TEXT, s_dist_07 TEXT, s_dist_08 TEXT, s_dist_09 TEXT, s_dist_10 TEXT,
  s_ytd INTEGER NOT NULL, s_order_cnt INTEGER NOT NULL, s_remote_cnt INTEGER NOT NULL, s_data TEXT, PRIMARY KEY (s_w_id, s_i_id));
CREATE TABLE order_line (ol_o_id INTEGER NOT NULL, ol_d_id INTEGER NOT NULL, ol_w_id INTEGER NOT NULL, ol_number INTEGER NOT NULL, ol_i_id INTEGER NOT NULL,
  ol_supply_w_id INTEGER, ol_delivery_d DATETIME, ol_quantity INTEGER NOT NULL, ol_amount DECIMAL(6,2) NOT NULL, ol_dist_info TEXT,
  PRIMARY KEY (ol_w_id, ol_d_id, ol_o_id, ol_number){olfk});
CREATE INDEX customer_name ON customer (c_w_id, c_d_id, c_last, c_first);
CREATE INDEX orders_customer ON orders (o_w_id, o_d_id, o_c_id, o_id);
",
        w = r("warehouse"),
        cfk = if fk { ", FOREIGN KEY (c_w_id, c_d_id) REFERENCES district (d_w_id, d_id)" } else { "" },
        hfk = if fk { ", FOREIGN KEY (h_c_w_id, h_c_d_id, h_c_id) REFERENCES customer (c_w_id, c_d_id, c_id), FOREIGN KEY (h_w_id, h_d_id) REFERENCES district (d_w_id, d_id)" } else { "" },
        ofk = if fk { ", FOREIGN KEY (o_w_id, o_d_id, o_c_id) REFERENCES customer (c_w_id, c_d_id, c_id)" } else { "" },
        nfk = if fk { ", FOREIGN KEY (no_w_id, no_d_id, no_o_id) REFERENCES orders (o_w_id, o_d_id, o_id)" } else { "" },
        si = r("item"),
        sw = r("warehouse"),
        olfk = if fk { ", FOREIGN KEY (ol_w_id, ol_d_id, ol_o_id) REFERENCES orders (o_w_id, o_d_id, o_id), FOREIGN KEY (ol_supply_w_id, ol_i_id) REFERENCES stock (s_w_id, s_i_id)" } else { "" },
    );
    s
}

const STAMP: &str = "2024-01-01 00:00:00";

fn load(db: &mut Database, cfg: &Cfg, seed: u64) {
    let mut r = Rng(seed | 1);
    let put = |db: &mut Database, table: &str, csv: &mut String| {
        if csv.is_empty() {
            return;
        }
        import_csv(db, table, csv, false).unwrap_or_else(|e| panic!("loading {table}: {e}"));
        csv.clear();
    };
    let mut s = String::new();
    for i in 1..=cfg.items {
        let _ = writeln!(s, "{i},{},{},{},{}", r.range(1, 10000), r.alpha(14, 24), money(r.range(100, 10000)), r.data(26, 50));
        if i % 20000 == 0 {
            put(db, "item", &mut s);
        }
    }
    put(db, "item", &mut s);
    let address = |r: &mut Rng| format!("{},{},{},{},{}", r.alpha(10, 20), r.alpha(10, 20), r.alpha(10, 20), r.alpha(2, 2).to_uppercase(), format!("{}11111", r.digits(4)));
    let district_ytd = cfg.customers * 10 * 100; // each customer has paid 10.00 so far
    for w in 1..=cfg.warehouses {
        let _ = writeln!(s, "{w},{},{},0.{:04},{}", r.alpha(6, 10), address(&mut r), r.range(0, 2000), money(district_ytd * DISTRICTS));
        put(db, "warehouse", &mut s);
        for i in 1..=cfg.items {
            let _ = write!(s, "{i},{w},{}", r.range(10, 100));
            for _ in 0..10 {
                let _ = write!(s, ",{}", r.alpha(24, 24));
            }
            let _ = writeln!(s, ",0,0,0,{}", r.data(26, 50));
            if i % 20000 == 0 {
                put(db, "stock", &mut s);
            }
        }
        put(db, "stock", &mut s);
        for d in 1..=DISTRICTS {
            let _ = writeln!(s, "{d},{w},{},{},0.{:04},{},{}", r.alpha(6, 10), address(&mut r), r.range(0, 2000), money(district_ytd), cfg.orders() + 1);
            put(db, "district", &mut s);
            let mut cust = String::new();
            let mut hist = String::new();
            for c in 1..=cfg.customers {
                let last = if c <= cfg.names() { last_name(c - 1) } else { last_name(r.nurand(255.min(pow2_below(cfg.names())), cfg.c_last, 0, cfg.names() - 1)) };
                let credit = if r.range(1, 10) == 1 { "BC" } else { "GC" };
                let _ = writeln!(
                    cust,
                    "{c},{d},{w},{},OE,{last},{},{},{STAMP},{credit},50000.00,0.{:04},-10.00,10.00,1,0,{}",
                    r.alpha(8, 16),
                    address(&mut r),
                    r.digits(16),
                    r.range(0, 5000),
                    r.alpha(300, 500)
                );
                let _ = writeln!(hist, "{c},{d},{w},{d},{w},{STAMP},10.00,{}", r.alpha(12, 24));
            }
            put(db, "customer", &mut cust);
            put(db, "history", &mut hist);
            // the first orders are delivered; the last `undelivered` are not, and are in new_order
            let mut perm: Vec<i64> = (1..=cfg.customers).collect();
            for i in (1..perm.len()).rev() {
                perm.swap(i, r.range(0, i as i64) as usize);
            }
            let (mut orders, mut lines, mut news) = (String::new(), String::new(), String::new());
            for o in 1..=cfg.orders() {
                let delivered = o <= cfg.orders() - cfg.undelivered();
                let ol_cnt = r.range(5, 15);
                let carrier = if delivered { r.range(1, 10).to_string() } else { String::new() };
                let _ = writeln!(orders, "{o},{d},{w},{},{STAMP},{carrier},{ol_cnt},1", perm[(o - 1) as usize]);
                for n in 1..=ol_cnt {
                    let (when, amount) = if delivered { (STAMP.to_string(), "0.00".to_string()) } else { (String::new(), money(r.range(1, 999999))) };
                    let _ = writeln!(lines, "{o},{d},{w},{n},{},{w},{when},5,{amount},{}", r.range(1, cfg.items), r.alpha(24, 24));
                }
                if !delivered {
                    let _ = writeln!(news, "{o},{d},{w}");
                }
            }
            put(db, "orders", &mut orders);
            put(db, "order_line", &mut lines);
            put(db, "new_order", &mut news);
        }
    }
}

// ----- the transactions --------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    NewOrder,
    Payment,
    OrderStatus,
    Delivery,
    StockLevel,
}

const KINDS: [Kind; 5] = [Kind::NewOrder, Kind::Payment, Kind::OrderStatus, Kind::Delivery, Kind::StockLevel];

fn name(k: Kind) -> &'static str {
    match k {
        Kind::NewOrder => "New-Order",
        Kind::Payment => "Payment",
        Kind::OrderStatus => "Order-Status",
        Kind::Delivery => "Delivery",
        Kind::StockLevel => "Stock-Level",
    }
}

#[derive(Default, Clone)]
struct Stats {
    done: [u64; 5],
    time: [Duration; 5],
    slowest: [Duration; 5],
    /// Runs that were told to start over, and what the second tries cost.
    retries: [u64; 5],
    /// New-Orders that rolled back by design (1% name an item that does not exist).
    rolled_back: u64,
    /// Transactions that gave up after too many retries.
    gave_up: u64,
    /// Orders and money the terminals saw committed, to be matched with the tables.
    orders: i64,
    paid: i64,
    delivered: i64,
}

impl Stats {
    fn add(&mut self, o: &Stats) {
        for i in 0..5 {
            self.done[i] += o.done[i];
            self.time[i] += o.time[i];
            self.slowest[i] = self.slowest[i].max(o.slowest[i]);
            self.retries[i] += o.retries[i];
        }
        self.rolled_back += o.rolled_back;
        self.gave_up += o.gave_up;
        self.orders += o.orders;
        self.paid += o.paid;
        self.delivered += o.delivered;
    }
}

struct Line {
    item: i64,
    supply: i64,
    qty: i64,
}

enum Params {
    NewOrder { w: i64, d: i64, c: i64, lines: Vec<Line>, bad_item: bool },
    Payment { w: i64, d: i64, cw: i64, cd: i64, by_name: Option<String>, c: i64, amount: i64 },
    OrderStatus { w: i64, d: i64, by_name: Option<String>, c: i64 },
    Delivery { w: i64, carrier: i64 },
    StockLevel { w: i64, d: i64, threshold: i64 },
}

fn params(cfg: &Cfg, r: &mut Rng, kind: Kind, w: i64) -> Params {
    let d = r.range(1, DISTRICTS);
    match kind {
        Kind::NewOrder => {
            let n = r.range(5, 15);
            let bad_item = r.range(1, 100) == 1;
            let lines = (0..n)
                .map(|k| Line {
                    item: if bad_item && k == n - 1 { cfg.items + 1 } else { cfg.pick_item(r) },
                    supply: if cfg.warehouses > 1 && r.range(1, 100) == 1 { other_warehouse(cfg, r, w) } else { w },
                    qty: r.range(1, 10),
                })
                .collect();
            Params::NewOrder { w, d, c: cfg.pick_customer(r), lines, bad_item }
        }
        Kind::Payment => {
            let remote = cfg.warehouses > 1 && r.range(1, 100) <= 15;
            let (cw, cd) = if remote { (other_warehouse(cfg, r, w), r.range(1, DISTRICTS)) } else { (w, d) };
            let by_name = (r.range(1, 100) <= 60).then(|| cfg.pick_last(r));
            Params::Payment { w, d, cw, cd, by_name, c: cfg.pick_customer(r), amount: r.range(100, 500000) }
        }
        Kind::OrderStatus => {
            let by_name = (r.range(1, 100) <= 60).then(|| cfg.pick_last(r));
            Params::OrderStatus { w, d, by_name, c: cfg.pick_customer(r) }
        }
        Kind::Delivery => Params::Delivery { w, carrier: r.range(1, 10) },
        Kind::StockLevel => Params::StockLevel { w, d, threshold: r.range(10, 20) },
    }
}

fn other_warehouse(cfg: &Cfg, r: &mut Rng, w: i64) -> i64 {
    loop {
        let o = r.range(1, cfg.warehouses);
        if o != w {
            return o;
        }
    }
}

type Done = Result<Outcome, Error>;

enum Outcome {
    Committed { orders: i64, paid: i64 },
    /// The transaction asked to be rolled back (New-Order with an unknown item).
    RolledBack,
}

fn committed() -> Done {
    Ok(Outcome::Committed { orders: 0, paid: 0 })
}

fn one(c: &Connection, sql: &str) -> Result<Vec<Value>, Error> {
    let mut r = c.execute(sql)?;
    if r.rows.is_empty() {
        return Err(Error::runtime(format!("no row for: {sql}")));
    }
    Ok(r.rows.swap_remove(0))
}

/// The customer of a Payment or Order-Status: by number, or the one in the middle of those with the last name.
fn find_customer(c: &Connection, w: i64, d: i64, by_name: &Option<String>, id: i64) -> Result<i64, Error> {
    let Some(last) = by_name else { return Ok(id) };
    let rows = c.execute(&format!("SELECT c_id FROM customer WHERE c_w_id = {w} AND c_d_id = {d} AND c_last = '{last}' ORDER BY c_first"))?.rows;
    if rows.is_empty() {
        return Err(Error::runtime(format!("no customer named {last}")));
    }
    Ok(int(&rows[(rows.len() + 1) / 2 - 1][0]))
}

/// `delivered` counts the districts a Delivery has committed, which stay committed when a later one has to start over.
fn run(cfg: &Cfg, c: &Connection, p: &Params, delivered: &mut i64) -> Done {
    let fu = cfg.lock_clause();
    match p {
        Params::NewOrder { w, d, c: cust, lines, bad_item } => {
            let _ = bad_item;
            c.execute("BEGIN")?;
            one(c, &format!("SELECT w_tax FROM warehouse WHERE w_id = {w}"))?;
            let dist = one(c, &format!("SELECT d_tax, d_next_o_id FROM district WHERE d_w_id = {w} AND d_id = {d}{fu}"))?;
            let o_id = int(&dist[1]);
            c.execute(&format!("UPDATE district SET d_next_o_id = {} WHERE d_w_id = {w} AND d_id = {d}", o_id + 1))?;
            one(c, &format!("SELECT c_discount, c_last, c_credit FROM customer WHERE c_w_id = {w} AND c_d_id = {d} AND c_id = {cust}"))?;
            let all_local = i64::from(lines.iter().all(|l| l.supply == *w));
            c.execute(&format!("INSERT INTO orders VALUES ({o_id}, {d}, {w}, {cust}, LOCALTIMESTAMP, NULL, {}, {all_local})", lines.len()))?;
            c.execute(&format!("INSERT INTO new_order VALUES ({o_id}, {d}, {w})"))?;
            for (n, l) in lines.iter().enumerate() {
                let item = c.execute(&format!("SELECT i_price, i_name, i_data FROM item WHERE i_id = {}", l.item))?;
                let Some(item) = item.rows.first() else {
                    c.execute("ROLLBACK")?;
                    return Ok(Outcome::RolledBack);
                };
                let price = cents(&item[0]);
                let st = one(c, &format!("SELECT s_quantity, s_dist_{d:02} FROM stock WHERE s_w_id = {} AND s_i_id = {}{fu}", l.supply, l.item))?;
                let q = int(&st[0]);
                let q = if q - l.qty >= 10 { q - l.qty } else { q - l.qty + 91 };
                c.execute(&format!(
                    "UPDATE stock SET s_quantity = {q}, s_ytd = s_ytd + {}, s_order_cnt = s_order_cnt + 1, s_remote_cnt = s_remote_cnt + {} WHERE s_w_id = {} AND s_i_id = {}",
                    l.qty,
                    i64::from(l.supply != *w),
                    l.supply,
                    l.item
                ))?;
                c.execute(&format!(
                    "INSERT INTO order_line VALUES ({o_id}, {d}, {w}, {}, {}, {}, NULL, {}, {}, '{}')",
                    n + 1,
                    l.item,
                    l.supply,
                    l.qty,
                    money(price * l.qty),
                    text(&st[1])
                ))?;
            }
            c.execute("COMMIT")?;
            Ok(Outcome::Committed { orders: 1, paid: 0 })
        }
        Params::Payment { w, d, cw, cd, by_name, c: cust, amount } => {
            c.execute("BEGIN")?;
            let h = money(*amount);
            one(c, &format!("SELECT w_ytd FROM warehouse WHERE w_id = {w}{fu}"))?;
            c.execute(&format!("UPDATE warehouse SET w_ytd = w_ytd + {h} WHERE w_id = {w}"))?;
            one(c, &format!("SELECT d_ytd FROM district WHERE d_w_id = {w} AND d_id = {d}{fu}"))?;
            c.execute(&format!("UPDATE district SET d_ytd = d_ytd + {h} WHERE d_w_id = {w} AND d_id = {d}"))?;
            let cid = find_customer(c, *cw, *cd, by_name, *cust)?;
            let row = one(c, &format!("SELECT c_credit FROM customer WHERE c_w_id = {cw} AND c_d_id = {cd} AND c_id = {cid}{fu}"))?;
            let data = if text(&row[0]) == "BC" { format!(", c_data = substr('{cid} {cd} {cw} {d} {w} {h}' || c_data, 1, 500)") } else { String::new() };
            c.execute(&format!(
                "UPDATE customer SET c_balance = c_balance - {h}, c_ytd_payment = c_ytd_payment + {h}, c_payment_cnt = c_payment_cnt + 1{data} \
                 WHERE c_w_id = {cw} AND c_d_id = {cd} AND c_id = {cid}"
            ))?;
            c.execute(&format!("INSERT INTO history VALUES ({cid}, {cd}, {cw}, {d}, {w}, LOCALTIMESTAMP, {h}, 'payment')"))?;
            c.execute("COMMIT")?;
            Ok(Outcome::Committed { orders: 0, paid: *amount })
        }
        Params::OrderStatus { w, d, by_name, c: cust } => {
            c.execute("BEGIN")?;
            let cid = find_customer(c, *w, *d, by_name, *cust)?;
            one(c, &format!("SELECT c_balance, c_first, c_last FROM customer WHERE c_w_id = {w} AND c_d_id = {d} AND c_id = {cid}"))?;
            let last = c.execute(&format!("SELECT o_id, o_entry_d, o_carrier_id, o_ol_cnt FROM orders WHERE o_w_id = {w} AND o_d_id = {d} AND o_c_id = {cid} ORDER BY o_id DESC LIMIT 1"))?;
            if let Some(o) = last.rows.first() {
                let lines = c.execute(&format!("SELECT ol_i_id, ol_supply_w_id, ol_quantity, ol_amount, ol_delivery_d FROM order_line WHERE ol_w_id = {w} AND ol_d_id = {d} AND ol_o_id = {}", int(&o[0])))?;
                if lines.rows.len() as i64 != int(&o[3]) {
                    return Err(Error::runtime(format!("order {} of customer {cid} has {} lines, not {}", int(&o[0]), lines.rows.len(), int(&o[3]))));
                }
            }
            c.execute("COMMIT")?;
            committed()
        }
        Params::Delivery { w, carrier } => {
            for d in 1..=DISTRICTS {
                c.execute("BEGIN")?;
                let oldest = c.execute(&format!("SELECT no_o_id FROM new_order WHERE no_w_id = {w} AND no_d_id = {d} ORDER BY no_o_id LIMIT 1{fu}"))?;
                let Some(o) = oldest.rows.first() else {
                    c.execute("COMMIT")?;
                    continue;
                };
                let o = int(&o[0]);
                c.execute(&format!("DELETE FROM new_order WHERE no_w_id = {w} AND no_d_id = {d} AND no_o_id = {o}"))?;
                let cust = int(&one(c, &format!("SELECT o_c_id FROM orders WHERE o_w_id = {w} AND o_d_id = {d} AND o_id = {o}{fu}"))?[0]);
                c.execute(&format!("UPDATE orders SET o_carrier_id = {carrier} WHERE o_w_id = {w} AND o_d_id = {d} AND o_id = {o}"))?;
                c.execute(&format!("UPDATE order_line SET ol_delivery_d = LOCALTIMESTAMP WHERE ol_w_id = {w} AND ol_d_id = {d} AND ol_o_id = {o}"))?;
                let total = cents(&one(c, &format!("SELECT SUM(ol_amount) FROM order_line WHERE ol_w_id = {w} AND ol_d_id = {d} AND ol_o_id = {o}"))?[0]);
                one(c, &format!("SELECT c_balance FROM customer WHERE c_w_id = {w} AND c_d_id = {d} AND c_id = {cust}{fu}"))?;
                c.execute(&format!(
                    "UPDATE customer SET c_balance = c_balance + {}, c_delivery_cnt = c_delivery_cnt + 1 WHERE c_w_id = {w} AND c_d_id = {d} AND c_id = {cust}",
                    money(total)
                ))?;
                c.execute("COMMIT")?;
                *delivered += 1;
            }
            committed()
        }
        Params::StockLevel { w, d, threshold } => {
            c.execute("BEGIN")?;
            let next = int(&one(c, &format!("SELECT d_next_o_id FROM district WHERE d_w_id = {w} AND d_id = {d}"))?[0]);
            one(
                c,
                &format!(
                    "SELECT COUNT(DISTINCT s_i_id) FROM order_line JOIN stock ON s_i_id = ol_i_id AND s_w_id = ol_w_id \
                     WHERE ol_w_id = {w} AND ol_d_id = {d} AND ol_o_id < {next} AND ol_o_id >= {} AND s_quantity < {threshold}",
                    next - 20
                ),
            )?;
            c.execute("COMMIT")?;
            committed()
        }
    }
}

/// Runs one transaction to its end, starting over each time it is told to.
fn transact(cfg: &Cfg, c: &Connection, r: &mut Rng, kind: Kind, p: &Params, st: &mut Stats) {
    let k = KINDS.iter().position(|x| *x == kind).unwrap();
    let started = Instant::now();
    let mut delivered = 0;
    for attempt in 0..200u64 {
        match run(cfg, c, p, &mut delivered) {
            Ok(Outcome::Committed { orders, paid }) => {
                let t = started.elapsed();
                st.done[k] += 1;
                st.time[k] += t;
                st.slowest[k] = st.slowest[k].max(t);
                st.orders += orders;
                st.paid += paid;
                st.delivered += delivered;
                return;
            }
            Ok(Outcome::RolledBack) => {
                st.rolled_back += 1;
                return;
            }
            Err(Error::Conflict(_) | Error::Locked(_)) => {
                let _ = c.execute("ROLLBACK");
                st.retries[k] += 1;
                std::thread::sleep(Duration::from_micros(r.range(0, 500 * (attempt as i64 + 1)) as u64));
            }
            Err(e) => {
                let _ = c.execute("ROLLBACK");
                eprintln!("{} failed: {e}", name(kind));
                st.gave_up += 1;
                st.delivered += delivered;
                return;
            }
        }
    }
    let _ = c.execute("ROLLBACK");
    st.gave_up += 1;
    st.delivered += delivered;
}

fn pick_kind(r: &mut Rng) -> Kind {
    match r.range(1, 100) {
        1..=45 => Kind::NewOrder,
        46..=88 => Kind::Payment,
        89..=92 => Kind::OrderStatus,
        93..=96 => Kind::Delivery,
        _ => Kind::StockLevel,
    }
}

// ----- the consistency conditions ----------------------------------------------------------------------------------

/// Clause 3.3.2: each query counts the rows that break the condition, so every answer should be 0.
const CONDITIONS: [(&str, &str); 10] = [
    ("1. w_ytd = sum(d_ytd)", "SELECT COUNT(*) FROM warehouse w WHERE w_ytd <> (SELECT SUM(d_ytd) FROM district d WHERE d.d_w_id = w.w_id)"),
    (
        "2. d_next_o_id - 1 = max(o_id) = max(no_o_id)",
        "SELECT COUNT(*) FROM district d WHERE d_next_o_id - 1 <> (SELECT MAX(o_id) FROM orders o WHERE o.o_w_id = d.d_w_id AND o.o_d_id = d.d_id)
           OR (SELECT MAX(no_o_id) FROM new_order n WHERE n.no_w_id = d.d_w_id AND n.no_d_id = d.d_id) <> d_next_o_id - 1",
    ),
    (
        "3. max(no_o_id) - min(no_o_id) + 1 = rows of new_order",
        "SELECT COUNT(*) FROM (SELECT no_w_id, no_d_id FROM new_order GROUP BY no_w_id, no_d_id HAVING MAX(no_o_id) - MIN(no_o_id) + 1 <> COUNT(*)) q",
    ),
    (
        "4. sum(o_ol_cnt) = rows of order_line",
        "SELECT COUNT(*) FROM district d WHERE (SELECT SUM(o_ol_cnt) FROM orders o WHERE o.o_w_id = d.d_w_id AND o.o_d_id = d.d_id)
           <> (SELECT COUNT(*) FROM order_line l WHERE l.ol_w_id = d.d_w_id AND l.ol_d_id = d.d_id)",
    ),
    (
        "5. an order has no carrier exactly when it is in new_order",
        "SELECT COUNT(*) FROM orders o WHERE (o_carrier_id IS NULL) <> EXISTS (SELECT 1 FROM new_order n WHERE n.no_w_id = o.o_w_id AND n.no_d_id = o.o_d_id AND n.no_o_id = o.o_id)",
    ),
    (
        "6. o_ol_cnt = lines of the order",
        "SELECT COUNT(*) FROM orders o WHERE o_ol_cnt <> (SELECT COUNT(*) FROM order_line l WHERE l.ol_w_id = o.o_w_id AND l.ol_d_id = o.o_d_id AND l.ol_o_id = o.o_id)",
    ),
    (
        "7. a line is delivered exactly when its order has a carrier",
        "SELECT COUNT(*) FROM order_line l JOIN orders o ON o.o_w_id = l.ol_w_id AND o.o_d_id = l.ol_d_id AND o.o_id = l.ol_o_id WHERE (l.ol_delivery_d IS NULL) <> (o.o_carrier_id IS NULL)",
    ),
    ("8. w_ytd = sum(h_amount)", "SELECT COUNT(*) FROM warehouse w WHERE w_ytd <> (SELECT SUM(h_amount) FROM history h WHERE h.h_w_id = w.w_id)"),
    ("9. d_ytd = sum(h_amount)", "SELECT COUNT(*) FROM district d WHERE d_ytd <> (SELECT SUM(h_amount) FROM history h WHERE h.h_w_id = d.d_w_id AND h.h_d_id = d.d_id)"),
    (
        "10. c_balance = delivered lines - payments;  12. c_balance + c_ytd_payment = delivered lines",
        "SELECT COUNT(*) FROM customer c WHERE c_balance <> COALESCE((SELECT SUM(ol_amount) FROM order_line l JOIN orders o ON o.o_w_id = l.ol_w_id AND o.o_d_id = l.ol_d_id AND o.o_id = l.ol_o_id
             WHERE o.o_w_id = c.c_w_id AND o.o_d_id = c.c_d_id AND o.o_c_id = c.c_id AND l.ol_delivery_d IS NOT NULL), 0)
             - (SELECT SUM(h_amount) FROM history h WHERE h.h_c_w_id = c.c_w_id AND h.h_c_d_id = c.c_d_id AND h.h_c_id = c.c_id)",
    ),
];

fn check(c: &Connection) -> usize {
    let mut broken = 0;
    for (label, sql) in CONDITIONS {
        match c.execute(sql) {
            Ok(r) => {
                let n = int(&r.rows[0][0]);
                println!("  {} {label}{}", if n == 0 { "ok  " } else { "FAIL" }, if n == 0 { String::new() } else { format!(": {n} rows break it") });
                broken += usize::from(n != 0);
            }
            Err(e) => {
                println!("  FAIL {label}: {e}");
                broken += 1;
            }
        }
    }
    broken
}

fn main() {
    let (mut warehouses, mut terminals, mut seconds, mut items, mut customers, mut seed) = (2i64, 8usize, 10u64, 10_000i64, 300i64, 1u64);
    let (mut lock, mut fk, mut explain) = (true, true, false);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut num = |flag: &str| it.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or_else(|| panic!("{flag} N"));
        match a.as_str() {
            "--warehouses" => warehouses = num("--warehouses"),
            "--terminals" => terminals = num("--terminals") as usize,
            "--seconds" => seconds = num("--seconds") as u64,
            "--items" => items = num("--items"),
            "--customers" => customers = num("--customers"),
            "--seed" => seed = num("--seed") as u64,
            "--no-lock" => lock = false,
            "--no-fk" => fk = false,
            "--explain" => explain = true,
            other => panic!("unknown argument {other}"),
        }
    }
    let mut seeder = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let cfg = Cfg { warehouses, items, customers, lock, c_last: seeder.range(0, 255), c_id: seeder.range(0, 1023), c_item: seeder.range(0, 8191) };

    let started = Instant::now();
    let mut db = Database::in_memory();
    db.execute(&schema(fk)).expect("the schema");
    load(&mut db, &cfg, seed);
    let shared = SharedDatabase::new(db);
    shared.set_busy_timeout(Some(Duration::from_secs(30)));
    println!(
        "{warehouses} warehouses, {items} items, {customers} customers per district, loaded in {:.1}s ({}, {})",
        started.elapsed().as_secs_f64(),
        if lock { "locking reads" } else { "no locking reads" },
        if fk { "foreign keys" } else { "no foreign keys" }
    );
    let control = shared.connect();
    println!("before the run:");
    if check(&control) != 0 {
        eprintln!("the loaded data is not consistent");
        std::process::exit(2);
    }

    let stop = AtomicBool::new(false);
    let begin = Instant::now();
    let mut total = Stats::default();
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..terminals)
            .map(|t| {
                let conn = shared.connect();
                let (cfg, stop) = (&cfg, &stop);
                s.spawn(move || {
                    let mut r = Rng((seed ^ (t as u64 + 1).wrapping_mul(0xD1B5_4A32_D192_ED03)) | 1);
                    let w = (t as i64 % cfg.warehouses) + 1;
                    let mut st = Stats::default();
                    while !stop.load(Ordering::Relaxed) {
                        let kind = pick_kind(&mut r);
                        let p = params(cfg, &mut r, kind, w);
                        transact(cfg, &conn, &mut r, kind, &p, &mut st);
                    }
                    st
                })
            })
            .collect();
        std::thread::sleep(Duration::from_secs(seconds));
        stop.store(true, Ordering::Relaxed);
        for h in handles {
            total.add(&h.join().expect("a terminal panicked"));
        }
    });
    let elapsed = begin.elapsed().as_secs_f64();

    println!("\n{terminals} terminals for {elapsed:.1}s:");
    println!("  {:<13}{:>9}{:>9}{:>12}{:>12}", "", "done", "retried", "mean ms", "slowest ms");
    for (i, k) in KINDS.iter().enumerate() {
        let mean = if total.done[i] > 0 { total.time[i].as_secs_f64() * 1000.0 / total.done[i] as f64 } else { 0.0 };
        println!("  {:<13}{:>9}{:>9}{:>12.1}{:>12.1}", name(*k), total.done[i], total.retries[i], mean, total.slowest[i].as_secs_f64() * 1000.0);
    }
    println!("  New-Order rolled back by design: {}; gave up: {}", total.rolled_back, total.gave_up);
    println!("  {:.0} New-Orders per minute (tpmC-like, with no think time)", total.done[0] as f64 * 60.0 / elapsed);

    if explain {
        // the Stock-Level query as it stands after the run, with its plan
        let next = int(&one(&control, "SELECT d_next_o_id FROM district WHERE d_w_id = 1 AND d_id = 1").unwrap()[0]);
        let q = format!(
            "SELECT COUNT(DISTINCT s_i_id) FROM order_line JOIN stock ON s_i_id = ol_i_id AND s_w_id = ol_w_id \
             WHERE ol_w_id = 1 AND ol_d_id = 1 AND ol_o_id < {next} AND ol_o_id >= {} AND s_quantity < 15",
            next - 20
        );
        let t = Instant::now();
        control.execute(&q).unwrap();
        println!("\nstock-level query: {:?}", t.elapsed());
        for touch in ["UPDATE stock SET s_ytd = s_ytd WHERE s_w_id = 1 AND s_i_id = 1", "UPDATE order_line SET ol_quantity = ol_quantity WHERE ol_w_id = 1 AND ol_d_id = 1 AND ol_o_id = 1 AND ol_number = 1"] {
            control.execute(touch).unwrap();
            let t = Instant::now();
            control.execute(&q).unwrap();
            println!("  after `{}`: {:?}", &touch[..touch.find(" SET").unwrap()], t.elapsed());
        }
        for row in control.execute(&format!("EXPLAIN {q}")).unwrap().rows {
            println!("  {}", row.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" | "));
        }
    }
    println!("\nafter the run:");
    let mut broken = check(&control);
    // what the terminals saw committed must be what the tables hold
    let next: i64 = int(&one(&control, "SELECT SUM(d_next_o_id) FROM district").unwrap()[0]);
    let orders = next - warehouses * DISTRICTS * (cfg.orders() + 1);
    let ytd = cents(&one(&control, "SELECT SUM(w_ytd) FROM warehouse").unwrap()[0]) - warehouses * DISTRICTS * cfg.customers * 1000;
    let undelivered = int(&one(&control, "SELECT COUNT(*) FROM new_order").unwrap()[0]);
    for (label, shown, expected) in [
        ("orders created", orders, total.orders),
        ("money paid (cents)", ytd, total.paid),
        ("orders delivered", warehouses * DISTRICTS * cfg.undelivered() + total.orders - undelivered, total.delivered),
    ] {
        let ok = shown == expected;
        println!("  {} {label}: tables {shown}, terminals {expected}", if ok { "ok  " } else { "FAIL" });
        broken += usize::from(!ok);
    }
    if broken == 0 && total.gave_up == 0 {
        println!("\nconsistent");
    } else {
        println!("\nNOT consistent ({broken} checks failed, {} transactions gave up)", total.gave_up);
        std::process::exit(1);
    }
}
