//! TPC-DS against Accessor: a generator in the shape of `dsdgen` (the official tool is not part of the repository, so the
//! values differ from the official answers) for the three sales channels with their returns, the inventory and the
//! dimensions, and a selection of the benchmark's queries (see `queries.rs`). Each query must run, finish in time, and
//! (with `--compare`) give the same rows with the optimizer off and on data without indexes.
//! With `--kit DIR` it instead loads the data of the official `dsdgen`, runs the 99 queries of `dsqgen` and compares each result with
//! the kit's answer set (see `kit.rs`). Usage: `accessor-tpcds [--kit DIR [--db FILE]] [--sf 0.01] [--seed N] [--compare] [--only 3,7] [--no-indexes] [--explain] [--timeout SECONDS] [--repl] [--exact-division] [--round-half-away]`.
mod kit;
mod queries;

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
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % (hi - lo + 1) as u64) as i64
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[(self.next() % items.len() as u64) as usize]
    }
    fn chance(&mut self, percent: i64) -> bool {
        self.range(1, 100) <= percent
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

fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

const STATES: [&str; 20] = ["TN", "SD", "AL", "SC", "OH", "GA", "MI", "IL", "CA", "WA", "CO", "TX", "OR", "MN", "KY", "VA", "MS", "NY", "FL", "IA"];
const COUNTIES: [&str; 8] = ["Williamson County", "Franklin Parish", "Bronx County", "Orange County", "Ziebach County", "Walker County", "Luce County", "Richland County"];
const CITIES: [&str; 8] = ["Midway", "Fairview", "Oakland", "Georgetown", "Salem", "Union", "Riverside", "Pleasant Hill"];
const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const CATEGORIES: [&str; 10] = ["Sports", "Books", "Home", "Music", "Women", "Men", "Children", "Electronics", "Jewelry", "Shoes"];
const CLASSES: [&str; 18] = [
    "personal", "portable", "reference", "self-help", "accessories", "classical", "fragrances", "pants", "computers", "stereo", "football", "shirts", "birdal",
    "dresses", "fiction", "history", "rock", "pop",
];
const COLORS: [&str; 10] = ["slate", "blanched", "burnished", "almond", "azure", "beige", "blue", "brown", "coral", "cyan"];
const STORE_NAMES: [&str; 6] = ["ese", "able", "ought", "bar", "eing", "anti"];
const EDUCATION: [&str; 7] = ["Primary", "Secondary", "College", "2 yr Degree", "4 yr Degree", "Advanced Degree", "Unknown"];
const BUY: [&str; 6] = [">10000", "5001-10000", "1001-5000", "501-1000", "0-500", "Unknown"];
const SALUTATIONS: [&str; 5] = ["Mr.", "Mrs.", "Ms.", "Dr.", "Sir"];
const NAMES: [&str; 12] = ["Smith", "Jones", "Brown", "Miller", "Davis", "Garcia", "Wilson", "Moore", "Taylor", "Clark", "Hall", "Young"];

fn money(cents: i64) -> String {
    format!("{}{}.{:02}", if cents < 0 { "-" } else { "" }, cents.abs() / 100, cents.abs() % 100)
}

const SCHEMA: &str = "
CREATE TABLE date_dim (d_date_sk INTEGER PRIMARY KEY, d_date_id TEXT, d_date DATE, d_month_seq INTEGER, d_week_seq INTEGER, d_quarter_seq INTEGER, d_year INTEGER,
  d_dow INTEGER, d_moy INTEGER, d_dom INTEGER, d_qoy INTEGER, d_day_name TEXT, d_quarter_name TEXT, d_holiday TEXT, d_weekend TEXT);
CREATE TABLE item (i_item_sk INTEGER PRIMARY KEY, i_item_id TEXT, i_item_desc TEXT, i_current_price DECIMAL(7,2), i_wholesale_cost DECIMAL(7,2), i_brand_id INTEGER, i_brand TEXT,
  i_class_id INTEGER, i_class TEXT, i_category_id INTEGER, i_category TEXT, i_manufact_id INTEGER, i_manufact TEXT, i_size TEXT, i_color TEXT, i_units TEXT, i_manager_id INTEGER, i_product_name TEXT);
CREATE TABLE customer_address (ca_address_sk INTEGER PRIMARY KEY, ca_address_id TEXT, ca_city TEXT, ca_county TEXT, ca_state TEXT, ca_zip TEXT, ca_country TEXT, ca_gmt_offset DECIMAL(5,2));
CREATE TABLE customer_demographics (cd_demo_sk INTEGER PRIMARY KEY, cd_gender TEXT, cd_marital_status TEXT, cd_education_status TEXT, cd_purchase_estimate INTEGER, cd_credit_rating TEXT, cd_dep_count INTEGER);
CREATE TABLE household_demographics (hd_demo_sk INTEGER PRIMARY KEY, hd_income_band_sk INTEGER, hd_buy_potential TEXT, hd_dep_count INTEGER, hd_vehicle_count INTEGER);
CREATE TABLE customer (c_customer_sk INTEGER PRIMARY KEY, c_customer_id TEXT, c_current_cdemo_sk INTEGER REFERENCES customer_demographics, c_current_hdemo_sk INTEGER REFERENCES household_demographics,
  c_current_addr_sk INTEGER REFERENCES customer_address, c_salutation TEXT, c_first_name TEXT, c_last_name TEXT, c_preferred_cust_flag TEXT, c_birth_year INTEGER, c_birth_country TEXT, c_email_address TEXT);
CREATE TABLE store (s_store_sk INTEGER PRIMARY KEY, s_store_id TEXT, s_store_name TEXT, s_number_employees INTEGER, s_company_id INTEGER, s_company_name TEXT, s_city TEXT, s_county TEXT,
  s_state TEXT, s_zip TEXT, s_gmt_offset DECIMAL(5,2));
CREATE TABLE promotion (p_promo_sk INTEGER PRIMARY KEY, p_promo_id TEXT, p_channel_email TEXT, p_channel_event TEXT, p_channel_dmail TEXT, p_channel_tv TEXT);
CREATE TABLE warehouse (w_warehouse_sk INTEGER PRIMARY KEY, w_warehouse_name TEXT, w_warehouse_sq_ft INTEGER, w_state TEXT);
CREATE TABLE web_site (web_site_sk INTEGER PRIMARY KEY, web_site_id TEXT, web_name TEXT, web_company_name TEXT);
CREATE TABLE reason (r_reason_sk INTEGER PRIMARY KEY, r_reason_desc TEXT);
CREATE TABLE store_sales (ss_sold_date_sk INTEGER, ss_item_sk INTEGER NOT NULL, ss_customer_sk INTEGER, ss_cdemo_sk INTEGER, ss_hdemo_sk INTEGER, ss_addr_sk INTEGER,
  ss_store_sk INTEGER, ss_promo_sk INTEGER, ss_ticket_number INTEGER NOT NULL, ss_quantity INTEGER, ss_wholesale_cost DECIMAL(7,2), ss_list_price DECIMAL(7,2), ss_sales_price DECIMAL(7,2),
  ss_ext_discount_amt DECIMAL(7,2), ss_ext_sales_price DECIMAL(7,2), ss_ext_wholesale_cost DECIMAL(7,2), ss_ext_list_price DECIMAL(7,2), ss_coupon_amt DECIMAL(7,2), ss_net_paid DECIMAL(7,2),
  ss_net_profit DECIMAL(7,2), PRIMARY KEY (ss_item_sk, ss_ticket_number));
CREATE TABLE store_returns (sr_returned_date_sk INTEGER, sr_item_sk INTEGER NOT NULL, sr_customer_sk INTEGER, sr_store_sk INTEGER, sr_reason_sk INTEGER, sr_ticket_number INTEGER NOT NULL,
  sr_return_quantity INTEGER, sr_return_amt DECIMAL(7,2), sr_net_loss DECIMAL(7,2), PRIMARY KEY (sr_item_sk, sr_ticket_number));
CREATE TABLE catalog_sales (cs_sold_date_sk INTEGER, cs_bill_customer_sk INTEGER, cs_bill_cdemo_sk INTEGER, cs_bill_addr_sk INTEGER, cs_warehouse_sk INTEGER, cs_item_sk INTEGER NOT NULL,
  cs_promo_sk INTEGER, cs_order_number INTEGER NOT NULL, cs_quantity INTEGER, cs_wholesale_cost DECIMAL(7,2), cs_list_price DECIMAL(7,2), cs_sales_price DECIMAL(7,2),
  cs_ext_discount_amt DECIMAL(7,2), cs_ext_sales_price DECIMAL(7,2), cs_ext_list_price DECIMAL(7,2), cs_coupon_amt DECIMAL(7,2), cs_net_paid DECIMAL(7,2), cs_net_profit DECIMAL(7,2),
  PRIMARY KEY (cs_item_sk, cs_order_number));
CREATE TABLE catalog_returns (cr_returned_date_sk INTEGER, cr_item_sk INTEGER NOT NULL, cr_order_number INTEGER NOT NULL, cr_return_amount DECIMAL(7,2), cr_net_loss DECIMAL(7,2),
  PRIMARY KEY (cr_item_sk, cr_order_number));
CREATE TABLE web_sales (ws_sold_date_sk INTEGER, ws_bill_customer_sk INTEGER, ws_bill_addr_sk INTEGER, ws_item_sk INTEGER NOT NULL, ws_web_site_sk INTEGER, ws_warehouse_sk INTEGER,
  ws_promo_sk INTEGER, ws_order_number INTEGER NOT NULL, ws_quantity INTEGER, ws_sales_price DECIMAL(7,2), ws_ext_discount_amt DECIMAL(7,2), ws_ext_sales_price DECIMAL(7,2),
  ws_ext_list_price DECIMAL(7,2), ws_net_paid DECIMAL(7,2), ws_net_profit DECIMAL(7,2), PRIMARY KEY (ws_item_sk, ws_order_number));
CREATE TABLE web_returns (wr_returned_date_sk INTEGER, wr_item_sk INTEGER NOT NULL, wr_order_number INTEGER NOT NULL, wr_return_amt DECIMAL(7,2), wr_net_loss DECIMAL(7,2),
  PRIMARY KEY (wr_item_sk, wr_order_number));
CREATE TABLE inventory (inv_date_sk INTEGER NOT NULL, inv_item_sk INTEGER NOT NULL, inv_warehouse_sk INTEGER NOT NULL, inv_quantity_on_hand INTEGER, PRIMARY KEY (inv_date_sk, inv_item_sk, inv_warehouse_sk));";

pub(crate) const INDEXES: &str = "
CREATE INDEX d_year ON date_dim (d_year, d_moy);
CREATE INDEX d_month_seq ON date_dim (d_month_seq);
CREATE INDEX d_date ON date_dim (d_date);
CREATE INDEX i_manufact ON item (i_manufact_id);
CREATE INDEX i_manager ON item (i_manager_id);
CREATE INDEX i_item_id ON item (i_item_id);
CREATE INDEX c_addr ON customer (c_current_addr_sk);
CREATE INDEX ss_date ON store_sales (ss_sold_date_sk);
CREATE INDEX ss_customer ON store_sales (ss_customer_sk);
CREATE INDEX ss_store ON store_sales (ss_store_sk);
CREATE INDEX sr_ticket ON store_returns (sr_customer_sk, sr_item_sk, sr_ticket_number);
CREATE INDEX cs_date ON catalog_sales (cs_sold_date_sk);
CREATE INDEX cs_customer ON catalog_sales (cs_bill_customer_sk, cs_item_sk);
CREATE INDEX cs_order ON catalog_sales (cs_order_number);
CREATE INDEX ws_order ON web_sales (ws_order_number);
CREATE INDEX ws_date ON web_sales (ws_sold_date_sk);
CREATE INDEX ws_customer ON web_sales (ws_bill_customer_sk);
CREATE INDEX inv_item ON inventory (inv_item_sk);";

const FIRST_DATE_SK: i64 = 2450815; // 1998-01-01

struct Sizes {
    items: i64,
    customers: i64,
    store_sales: i64,
    catalog_sales: i64,
    web_sales: i64,
    inventory: i64,
}

fn load(db: &mut Database, sf: f64, seed: u64) -> Sizes {
    let mut r = Rng(seed | 1);
    let n_item = ((18_000.0 * sf) as i64).max(300);
    let n_cust = ((100_000.0 * sf) as i64).max(1000);
    let n_addr = n_cust / 2;
    let n_cdemo = 2000;
    let n_hdemo = 720;
    let n_store = 12;
    let n_promo = 50;
    let n_wh = 5;
    let n_site = 6;
    let n_reason = 35;
    let put = |db: &mut Database, table: &str, csv: &mut String| {
        import_csv(db, table, csv, false).unwrap_or_else(|e| panic!("loading {table}: {e}"));
        csv.clear();
    };
    let mut s = String::new();

    let (first, last) = (days(1998, 1, 1), days(2003, 12, 31));
    let n_dates = last - first + 1;
    for i in 0..n_dates {
        let z = first + i;
        let (y, m, d) = civil(z);
        let dow = (z + 4).rem_euclid(7);
        let sk = FIRST_DATE_SK + i;
        let _ = writeln!(
            s,
            "{sk},AAAAAAAA{sk},{y:04}-{m:02}-{d:02},{},{},{},{y},{dow},{m},{d},{},{},{y}Q{},N,{}",
            (y - 1900) * 12 + m - 1,
            (z + 3).div_euclid(7) + 1 - 0,
            (y - 1900) * 4 + (m - 1) / 3 + 1,
            (m - 1) / 3 + 1,
            DAYS[dow as usize],
            (m - 1) / 3 + 1,
            if dow == 0 || dow == 6 { "Y" } else { "N" }
        );
    }
    put(db, "date_dim", &mut s);

    for k in 1..=n_item {
        let cat = r.range(0, 9);
        let price = r.range(9, 9999);
        let _ = writeln!(
            s,
            "{k},AAAAAAAA{:08},item description {k},{},{},{},brandbrand #{},{},{},{},{},{},manufact{},{},{},Each,{},product{k}",
            (k + 1) / 2,
            money(price),
            money(price * r.range(30, 90) / 100),
            r.range(1, 1000) * 1000 + r.range(1, 9),
            r.range(1, 100),
            r.range(1, 18),
            r.pick(&CLASSES),
            cat + 1,
            CATEGORIES[cat as usize],
            r.range(1, 400),
            r.range(1, 400),
            r.pick(&["small", "medium", "large", "extra large", "petite", "N/A"]),
            r.pick(&COLORS),
            r.range(1, 100)
        );
    }
    // the class and class id above are independent draws; the id is not used by the queries
    put(db, "item", &mut s);

    for k in 1..=n_addr {
        let _ = writeln!(
            s,
            "{k},AAAAAAAA{k},{},{},{},{:05},United States,{}",
            r.pick(&CITIES),
            r.pick(&COUNTIES),
            r.pick(&STATES),
            r.range(80000, 89999),
            r.pick(&["-5", "-6", "-7", "-8"])
        );
    }
    put(db, "customer_address", &mut s);

    for k in 1..=n_cdemo {
        let _ = writeln!(s, "{k},{},{},{},{},{},{}", r.pick(&["M", "F"]), r.pick(&["M", "S", "D", "W", "U"]), r.pick(&EDUCATION), r.range(1, 20) * 500, r.pick(&["Good", "High Risk", "Low Risk", "Unknown"]), r.range(0, 6));
    }
    put(db, "customer_demographics", &mut s);
    for k in 1..=n_hdemo {
        let _ = writeln!(s, "{k},{},{},{},{}", r.range(1, 20), r.pick(&BUY), r.range(0, 9), r.range(-1, 4));
    }
    put(db, "household_demographics", &mut s);

    for k in 1..=n_cust {
        // some customers have no address or demographics: the sales then carry a NULL (an empty field)
        let addr = if r.chance(3) { String::new() } else { r.range(1, n_addr).to_string() };
        let _ = writeln!(
            s,
            "{k},AAAAAAAA{k:08},{},{},{addr},{},{},{},{},{},UNITED STATES,user{k}@example.com",
            r.range(1, n_cdemo),
            r.range(1, n_hdemo),
            r.pick(&SALUTATIONS),
            r.pick(&["John", "Mary", "James", "Linda", "Robert", "Susan", "Paul", "Anna"]),
            r.pick(&NAMES),
            r.pick(&["Y", "N"]),
            r.range(1930, 1990)
        );
    }
    put(db, "customer", &mut s);

    for k in 1..=n_store {
        let _ = writeln!(
            s,
            "{k},AAAAAAAA{k:08},{},{},{},Company {},{},{},{},{:05},{}",
            STORE_NAMES[((k - 1) % 6) as usize],
            r.range(200, 300),
            r.range(1, 6),
            r.range(1, 6),
            r.pick(&CITIES),
            r.pick(&COUNTIES),
            r.pick(&STATES),
            r.range(80000, 89999),
            if r.chance(80) { "-5" } else { "-6" }
        );
    }
    put(db, "store", &mut s);
    for k in 1..=n_promo {
        let _ = writeln!(s, "{k},AAAAAAAA{k:08},{},{},{},{}", r.pick(&["Y", "N"]), r.pick(&["Y", "N"]), r.pick(&["Y", "N"]), r.pick(&["Y", "N"]));
    }
    put(db, "promotion", &mut s);
    for k in 1..=n_wh {
        let _ = writeln!(s, "{k},Warehouse {k},{},{}", r.range(50000, 1000000), r.pick(&STATES));
    }
    put(db, "warehouse", &mut s);
    for k in 1..=n_site {
        let _ = writeln!(s, "{k},AAAAAAAA{k:08},site_{},Company {}", k - 1, r.range(1, 6));
    }
    put(db, "web_site", &mut s);
    for k in 1..=n_reason {
        let _ = writeln!(s, "{k},reason {k}");
    }
    put(db, "reason", &mut s);

    let date = |r: &mut Rng| FIRST_DATE_SK + r.range(0, n_dates - 1);
    let opt = |r: &mut Rng, v: i64| if r.chance(2) { String::new() } else { v.to_string() };
    let prices = |r: &mut Rng| {
        let wholesale = r.range(100, 10000);
        let list = wholesale * r.range(100, 200) / 100;
        let sales = list * r.range(30, 100) / 100;
        (wholesale, list, sales)
    };

    // store sales: tickets of 1 to 7 different items
    let mut sales_lines: Vec<(i64, i64, i64, i64)> = Vec::new(); // ticket, item, customer, date
    let mut returns = String::new();
    let mut ticket = 0;
    let mut rows = 0;
    let target = ((2_880_000.0 * sf) as i64).max(2000);
    while rows < target {
        ticket += 1;
        let (d, c, a, cd, hd, st) = (date(&mut r), r.range(1, n_cust), r.range(1, n_addr), r.range(1, n_cdemo), r.range(1, n_hdemo), r.range(1, n_store));
        let n = r.range(1, 7);
        let mut used = Vec::new();
        for _ in 0..n {
            let it = r.range(1, n_item);
            if used.contains(&it) {
                continue;
            }
            used.push(it);
            rows += 1;
            let q = r.range(1, 100);
            let (w, l, p) = prices(&mut r);
            let coupon = if r.chance(20) { p * q / 10 } else { 0 };
            let _ = writeln!(
                s,
                "{},{it},{},{cd},{hd},{a},{st},{},{ticket},{q},{},{},{},{},{},{},{},{},{},{}",
                opt(&mut r, d),
                opt(&mut r, c),
                r.range(1, n_promo),
                money(w),
                money(l),
                money(p),
                money((l - p) * q),
                money(p * q),
                money(w * q),
                money(l * q),
                money(coupon),
                money(p * q - coupon),
                money(p * q - w * q)
            );
            sales_lines.push((ticket, it, c, d));
        }
    }
    put(db, "store_sales", &mut s);
    for &(t, it, c, d) in &sales_lines {
        if r.chance(10) {
            let q = r.range(1, 20);
            let amt = r.range(100, 20000);
            let _ = writeln!(returns, "{},{it},{c},{},{},{t},{q},{},{}", d + r.range(1, 90), r.range(1, n_store), r.range(1, n_reason), money(amt), money(r.range(1, 500)));
        }
    }
    put(db, "store_returns", &mut returns);

    // catalog sales, with returns
    let mut orders: Vec<(i64, i64)> = Vec::new();
    let (mut order, mut rows) = (0, 0);
    let target = ((1_440_000.0 * sf) as i64).max(1000);
    let mut cr = String::new();
    while rows < target {
        order += 1;
        let (d, c, a, cd, wh) = (date(&mut r), r.range(1, n_cust), r.range(1, n_addr), r.range(1, n_cdemo), r.range(1, n_wh));
        let mut used = Vec::new();
        for _ in 0..r.range(1, 6) {
            let it = r.range(1, n_item);
            if used.contains(&it) {
                continue;
            }
            used.push(it);
            rows += 1;
            let q = r.range(1, 100);
            let (w, l, p) = prices(&mut r);
            let _ = writeln!(
                s,
                "{d},{c},{cd},{a},{wh},{it},{},{order},{q},{},{},{},{},{},{},{},{},{}",
                r.range(1, n_promo),
                money(w),
                money(l),
                money(p),
                money((l - p) * q),
                money(p * q),
                money(l * q),
                money(if r.chance(10) { p * q / 10 } else { 0 }),
                money(p * q),
                money(p * q - w * q)
            );
            orders.push((order, it));
            if r.chance(10) {
                let _ = writeln!(cr, "{},{it},{order},{},{}", d + r.range(1, 90), money(r.range(100, 20000)), money(r.range(1, 500)));
            }
        }
    }
    put(db, "catalog_sales", &mut s);
    put(db, "catalog_returns", &mut cr);

    // web sales, with returns
    let (mut order, mut rows) = (0, 0);
    let target = ((720_000.0 * sf) as i64).max(500);
    let mut wr = String::new();
    while rows < target {
        order += 1;
        let (d, c, a, site, wh) = (date(&mut r), r.range(1, n_cust), r.range(1, n_addr), r.range(1, n_site), r.range(1, n_wh));
        let mut used = Vec::new();
        for _ in 0..r.range(1, 6) {
            let it = r.range(1, n_item);
            if used.contains(&it) {
                continue;
            }
            used.push(it);
            rows += 1;
            let q = r.range(1, 100);
            let (w, l, p) = prices(&mut r);
            let _ = writeln!(
                s,
                "{d},{c},{a},{it},{site},{wh},{},{order},{q},{},{},{},{},{},{}",
                r.range(1, n_promo),
                money(p),
                money((l - p) * q),
                money(p * q),
                money(l * q),
                money(p * q),
                money(p * q - w * q)
            );
            if r.chance(10) {
                let _ = writeln!(wr, "{},{it},{order},{},{}", d + r.range(1, 90), money(r.range(100, 20000)), money(r.range(1, 500)));
            }
        }
    }
    put(db, "web_sales", &mut s);
    put(db, "web_returns", &mut wr);

    // inventory: a snapshot every week of 2000 and 2001, for part of the items
    let mut inv_rows = 0;
    let step = (n_item / 400).max(1);
    let mut week = days(2000, 1, 6);
    while week <= days(2001, 12, 31) {
        let sk = FIRST_DATE_SK + (week - first);
        for it in (1..=n_item).step_by(step as usize) {
            for wh in 1..=n_wh {
                let _ = writeln!(s, "{sk},{it},{wh},{}", r.range(0, 1000));
                inv_rows += 1;
            }
        }
        week += 7;
    }
    put(db, "inventory", &mut s);
    Sizes { items: n_item, customers: n_cust, store_sales: rows_of(sales_lines.len()), catalog_sales: rows_of(orders.len()), web_sales: order, inventory: inv_rows }
}

fn rows_of(n: usize) -> i64 {
    n as i64
}

pub(crate) fn run(db: &mut Database, sql: &str) -> Result<(Vec<String>, Duration), String> {
    let t = Instant::now();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| db.execute(sql))) {
        Err(_) => Err("PANIC".into()),
        Ok(Err(e)) => Err(e.to_string()),
        Ok(Ok(r)) => Ok((r.rows.iter().map(|row| row.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("|")).collect(), t.elapsed())),
    }
}

fn main() {
    let mut explain = false;
    let mut repl = false;
    let mut settings: Vec<String> = Vec::new();
    let mut kit: Option<String> = None;
    let mut db_file: Option<String> = None;
    let mut extra: Vec<String> = Vec::new();
    let (mut sf, mut seed, mut compare, mut only, mut indexes, mut timeout) = (0.01f64, 1u64, false, Vec::<u32>::new(), true, 120u64);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--sf" => sf = it.next().and_then(|v| v.parse().ok()).expect("--sf N"),
            "--seed" => seed = it.next().and_then(|v| v.parse().ok()).expect("--seed N"),
            "--compare" => compare = true,
            "--explain" => explain = true,
            "--repl" => repl = true,
            "--exact-division" => settings.push("SET integer_division = off".to_string()),
            "--round-half-away" => settings.push("SET round_half_even = off".to_string()),
            "--db" => db_file = Some(it.next().expect("--db FILE").clone()),
            "--kit" => kit = Some(it.next().expect("--kit DIR").clone()),
            "--sql" => extra.push(it.next().expect("--sql QUERY").clone()),
            "--no-indexes" => indexes = false,
            "--timeout" => timeout = it.next().and_then(|v| v.parse().ok()).expect("--timeout SECONDS"),
            "--only" => only = it.next().expect("--only 1,2").split(',').filter_map(|v| v.parse().ok()).collect(),
            other => panic!("unknown argument {other}"),
        }
    }
    if let Some(dir) = kit {
        let bad = kit::main(&dir, db_file.as_deref(), &only, indexes, timeout, explain, &extra, repl, &settings);
        std::process::exit(if bad > 0 { 1 } else { 0 });
    }
    let mut db = Database::in_memory();
    let started = Instant::now();
    db.execute(SCHEMA).expect("the schema");
    let sizes = load(&mut db, sf, seed);
    if indexes {
        db.execute(INDEXES).expect("the indexes");
    }
    println!(
        "scale factor {sf}: {} items, {} customers, {} store sales lines, {} catalog sales lines, {} web orders, {} inventory rows, loaded in {:.1}s{}",
        sizes.items,
        sizes.customers,
        sizes.store_sales,
        sizes.catalog_sales,
        sizes.web_sales,
        sizes.inventory,
        started.elapsed().as_secs_f64(),
        if indexes { "" } else { " (no indexes)" }
    );
    let _ = db.execute(&format!("SET statement_timeout = '{timeout}s'"));
    let mut twin = Database::in_memory();
    if compare && indexes {
        twin.execute(SCHEMA).expect("the schema");
        load(&mut twin, sf, seed);
        let _ = twin.execute(&format!("SET statement_timeout = '{timeout}s'"));
    }
    std::panic::set_hook(Box::new(|_| {}));
    for sql in &extra {
        match run(&mut db, sql) {
            Ok((rows, took)) => println!("{:.3}s, {} rows, first: {}", took.as_secs_f64(), rows.len(), rows.first().cloned().unwrap_or_default()),
            Err(e) => println!("FAILED: {e}"),
        }
    }
    if !extra.is_empty() {
        return;
    }
    let mut bad = 0;
    println!("{:>3} {:>7} {:>9}  {}", "Q", "rows", "seconds", "result");
    for (n, sql) in queries::queries() {
        if !only.is_empty() && !only.contains(&n) {
            continue;
        }
        if explain {
            match run(&mut db, &format!("EXPLAIN {sql}")) {
                Ok((plan, _)) => println!("{n}:\n{}", plan.join("\n")),
                Err(e) => println!("{n}: EXPLAIN failed: {e}"),
            }
            continue;
        }
        match run(&mut db, sql) {
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
                    // with the optimizer off these queries are cross joins of the whole schema: far too slow to wait for
                    let mut variants: Vec<(&str, bool)> = Vec::new();
                    if indexes {
                        variants.push(("no indexes", true));
                    }
                    for (what, use_twin) in variants {
                        let d = if use_twin { &mut twin } else { &mut db };
                                                let other = run(d, sql);
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
