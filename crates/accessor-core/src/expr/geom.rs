//! Spatial values. A geometry is kept as its canonical WKT text (`POINT(10 10)`), so a `POINT` or `GEOMETRY` column
//! is a text column and compares, sorts and prints by that text. The functions here parse the text, do the geometry
//! and write the answer back: the constructors (`POINT`, `LINESTRING`, `ST_GEOMFROMTEXT`..), the accessors (`ST_X`,
//! `ST_NUMPOINTS`..), `ST_AREA`, `ST_LENGTH`, `ST_DISTANCE`, and the relations (`ST_CONTAINS`.., `MBRCONTAINS`..).
//! The relations are exact for the usual shapes (points, lines and simple polygons) and approximate for exotic ones.

use std::borrow::Cow;

use crate::error::{Error, Result};
use crate::value::Value;

type P = (f64, f64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Point,
    Line,
    Polygon,
    Collection,
}

#[derive(Clone, Debug, PartialEq)]
enum Geom {
    Point(P),
    Line(Vec<P>),
    Polygon(Vec<Vec<P>>),
    Multi(Kind, Vec<Geom>),
}

/// The names this module answers to, with the number of arguments each takes.
pub const NAMES: &[(&str, usize, usize)] = &[
    ("ST_GEOMFROMTEXT", 1, 3),
    ("ST_GEOMETRYFROMTEXT", 1, 3),
    ("GEOMFROMTEXT", 1, 3),
    ("GEOMETRYFROMTEXT", 1, 3),
    ("ST_POINTFROMTEXT", 1, 3),
    ("POINTFROMTEXT", 1, 3),
    ("ST_LINEFROMTEXT", 1, 3),
    ("ST_LINESTRINGFROMTEXT", 1, 3),
    ("LINEFROMTEXT", 1, 3),
    ("LINESTRINGFROMTEXT", 1, 3),
    ("ST_POLYFROMTEXT", 1, 3),
    ("ST_POLYGONFROMTEXT", 1, 3),
    ("POLYFROMTEXT", 1, 3),
    ("POLYGONFROMTEXT", 1, 3),
    ("ST_MPOINTFROMTEXT", 1, 3),
    ("ST_MULTIPOINTFROMTEXT", 1, 3),
    ("MPOINTFROMTEXT", 1, 3),
    ("MULTIPOINTFROMTEXT", 1, 3),
    ("ST_MLINEFROMTEXT", 1, 3),
    ("ST_MULTILINESTRINGFROMTEXT", 1, 3),
    ("MLINEFROMTEXT", 1, 3),
    ("MULTILINESTRINGFROMTEXT", 1, 3),
    ("ST_MPOLYFROMTEXT", 1, 3),
    ("ST_MULTIPOLYGONFROMTEXT", 1, 3),
    ("MPOLYFROMTEXT", 1, 3),
    ("MULTIPOLYGONFROMTEXT", 1, 3),
    ("ST_GEOMCOLLFROMTEXT", 1, 3),
    ("ST_GEOMETRYCOLLECTIONFROMTEXT", 1, 3),
    ("GEOMCOLLFROMTEXT", 1, 3),
    ("GEOMETRYCOLLECTIONFROMTEXT", 1, 3),
    ("ST_ASTEXT", 1, 2),
    ("ST_ASWKT", 1, 2),
    ("POINT", 2, 2),
    ("LINESTRING", 1, usize::MAX),
    ("POLYGON", 1, usize::MAX),
    ("MULTIPOINT", 1, usize::MAX),
    ("MULTILINESTRING", 1, usize::MAX),
    ("MULTIPOLYGON", 1, usize::MAX),
    ("GEOMETRYCOLLECTION", 0, usize::MAX),
    ("ST_X", 1, 2),
    ("ST_Y", 1, 2),
    ("ST_SRID", 1, 2),
    ("ST_GEOMETRYTYPE", 1, 1),
    ("ST_DIMENSION", 1, 1),
    ("ST_NUMPOINTS", 1, 1),
    ("ST_NUMGEOMETRIES", 1, 1),
    ("ST_NUMINTERIORRINGS", 1, 1),
    ("ST_NUMINTERIORRING", 1, 1),
    ("ST_GEOMETRYN", 2, 2),
    ("ST_POINTN", 2, 2),
    ("ST_STARTPOINT", 1, 1),
    ("ST_ENDPOINT", 1, 1),
    ("ST_EXTERIORRING", 1, 1),
    ("ST_INTERIORRINGN", 2, 2),
    ("ST_ISEMPTY", 1, 1),
    ("ST_ISCLOSED", 1, 1),
    ("ST_ISSIMPLE", 1, 1),
    ("ST_ISVALID", 1, 1),
    ("ST_LENGTH", 1, 2),
    ("GLENGTH", 1, 1),
    ("ST_AREA", 1, 2),
    ("ST_ENVELOPE", 1, 1),
    ("ST_CENTROID", 1, 1),
    ("ST_DISTANCE", 2, 3),
    ("ST_CONTAINS", 2, 2),
    ("ST_WITHIN", 2, 2),
    ("ST_INTERSECTS", 2, 2),
    ("ST_DISJOINT", 2, 2),
    ("ST_EQUALS", 2, 2),
    ("ST_TOUCHES", 2, 2),
    ("ST_OVERLAPS", 2, 2),
    ("ST_CROSSES", 2, 2),
    ("MBRCONTAINS", 2, 2),
    ("MBRWITHIN", 2, 2),
    ("MBRCOVEREDBY", 2, 2),
    ("MBRCOVERS", 2, 2),
    ("MBRINTERSECTS", 2, 2),
    ("MBRDISJOINT", 2, 2),
    ("MBREQUALS", 2, 2),
    ("MBRTOUCHES", 2, 2),
    ("MBROVERLAPS", 2, 2),
];

/// The registered name (with its static lifetime) of a function of this module.
pub fn lookup(name: &str) -> Option<(&'static str, usize, usize)> {
    NAMES.iter().find(|(n, _, _)| *n == name).copied()
}

// ----- text <-> geometry -----

struct Reader<'a> {
    s: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn word(&mut self) -> String {
        self.ws();
        let start = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_alphabetic() || self.s[self.i] == b'_') {
            self.i += 1;
        }
        String::from_utf8_lossy(&self.s[start..self.i]).to_ascii_uppercase()
    }
    fn num(&mut self) -> Option<f64> {
        self.ws();
        let start = self.i;
        while self.i < self.s.len() && matches!(self.s[self.i], b'0'..=b'9' | b'.' | b'-' | b'+' | b'e' | b'E') {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok().filter(|f: &f64| f.is_finite())
    }
    fn point(&mut self) -> Option<P> {
        let x = self.num()?;
        let y = self.num()?;
        Some((x, y))
    }
    /// `(x y, x y, ..)`
    fn points(&mut self) -> Option<Vec<P>> {
        if !self.eat(b'(') {
            return None;
        }
        let mut out = vec![self.point()?];
        while self.eat(b',') {
            out.push(self.point()?);
        }
        self.eat(b')').then_some(out)
    }
    fn empty(&mut self) -> bool {
        let save = self.i;
        if self.word() == "EMPTY" {
            true
        } else {
            self.i = save;
            false
        }
    }
    fn list<T>(&mut self, item: &mut dyn FnMut(&mut Self) -> Option<T>) -> Option<Vec<T>> {
        if !self.eat(b'(') {
            return None;
        }
        let mut out = vec![item(self)?];
        while self.eat(b',') {
            out.push(item(self)?);
        }
        self.eat(b')').then_some(out)
    }
    fn polygon_body(&mut self) -> Option<Vec<Vec<P>>> {
        self.list(&mut |r: &mut Self| r.points())
    }
    fn geom(&mut self) -> Option<Geom> {
        let w = self.word();
        match w.as_str() {
            "POINT" => {
                if !self.eat(b'(') {
                    return None;
                }
                let p = self.point()?;
                self.eat(b')').then_some(Geom::Point(p))
            }
            "LINESTRING" => {
                if self.empty() {
                    return Some(Geom::Line(Vec::new()));
                }
                self.points().map(Geom::Line)
            }
            "POLYGON" => {
                if self.empty() {
                    return Some(Geom::Polygon(Vec::new()));
                }
                self.polygon_body().map(Geom::Polygon)
            }
            "MULTIPOINT" => {
                if self.empty() {
                    return Some(Geom::Multi(Kind::Point, Vec::new()));
                }
                let items = self.list(&mut |r: &mut Self| if r.eat(b'(') { r.point().filter(|_| r.eat(b')')) } else { r.point() })?;
                Some(Geom::Multi(Kind::Point, items.into_iter().map(Geom::Point).collect()))
            }
            "MULTILINESTRING" => {
                if self.empty() {
                    return Some(Geom::Multi(Kind::Line, Vec::new()));
                }
                let items = self.list(&mut |r: &mut Self| r.points())?;
                Some(Geom::Multi(Kind::Line, items.into_iter().map(Geom::Line).collect()))
            }
            "MULTIPOLYGON" => {
                if self.empty() {
                    return Some(Geom::Multi(Kind::Polygon, Vec::new()));
                }
                let items = self.list(&mut |r: &mut Self| r.polygon_body())?;
                Some(Geom::Multi(Kind::Polygon, items.into_iter().map(Geom::Polygon).collect()))
            }
            "GEOMETRYCOLLECTION" => {
                if self.empty() {
                    return Some(Geom::Multi(Kind::Collection, Vec::new()));
                }
                let items = self.list(&mut |r: &mut Self| r.geom())?;
                Some(Geom::Multi(Kind::Collection, items))
            }
            _ => None,
        }
    }
}

fn valid(g: &Geom) -> bool {
    match g {
        Geom::Point(_) => true,
        Geom::Line(p) => p.is_empty() || p.len() >= 2,
        Geom::Polygon(rings) => rings.iter().all(|r| r.len() >= 4 && r.first() == r.last()),
        Geom::Multi(_, items) => items.iter().all(valid),
    }
}

fn parse(s: &str) -> Option<Geom> {
    let mut r = Reader { s: s.as_bytes(), i: 0 };
    let g = r.geom()?;
    r.ws();
    (r.i == s.len() && valid(&g)).then_some(g)
}

fn pt(p: P) -> String {
    format!("{} {}", p.0, p.1)
}

fn pts(v: &[P]) -> String {
    v.iter().map(|p| pt(*p)).collect::<Vec<_>>().join(",")
}

fn rings(r: &[Vec<P>]) -> String {
    r.iter().map(|x| format!("({})", pts(x))).collect::<Vec<_>>().join(",")
}

fn write(g: &Geom) -> String {
    match g {
        Geom::Point(p) => format!("POINT({})", pt(*p)),
        Geom::Line(p) if p.is_empty() => "LINESTRING EMPTY".into(),
        Geom::Line(p) => format!("LINESTRING({})", pts(p)),
        Geom::Polygon(r) if r.is_empty() => "POLYGON EMPTY".into(),
        Geom::Polygon(r) => format!("POLYGON({})", rings(r)),
        Geom::Multi(k, items) => {
            let name = match k {
                Kind::Point => "MULTIPOINT",
                Kind::Line => "MULTILINESTRING",
                Kind::Polygon => "MULTIPOLYGON",
                Kind::Collection => "GEOMETRYCOLLECTION",
            };
            if items.is_empty() {
                return format!("{name} EMPTY");
            }
            let body: Vec<String> = items
                .iter()
                .map(|i| match (k, i) {
                    (Kind::Point, Geom::Point(p)) => format!("({})", pt(*p)),
                    (Kind::Line, Geom::Line(p)) => format!("({})", pts(p)),
                    (Kind::Polygon, Geom::Polygon(r)) => format!("({})", rings(r)),
                    _ => write(i),
                })
                .collect();
            format!("{name}({})", body.join(","))
        }
    }
}

fn text_of(v: &Value) -> String {
    match v {
        Value::Text(s) => s.to_string(),
        other => other.to_string(),
    }
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Double(d) => Some(*d),
        other => text_of(other).trim().parse().ok(),
    }
}

fn geom_of(fname: &str, v: &Value) -> Result<Geom> {
    parse(&text_of(v)).ok_or_else(|| Error::runtime(format!("Cannot get geometry object from data you send to the GEOMETRY field ({fname})")))
}

fn out(g: &Geom) -> Value {
    Value::text(&write(g))
}

// ----- geometry arithmetic -----

fn orient(a: P, b: P, c: P) -> f64 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

fn on_seg(p: P, a: P, b: P) -> bool {
    orient(a, b, p) == 0.0 && p.0 >= a.0.min(b.0) && p.0 <= a.0.max(b.0) && p.1 >= a.1.min(b.1) && p.1 <= a.1.max(b.1)
}

fn segs_meet(a: P, b: P, c: P, d: P) -> bool {
    let (o1, o2, o3, o4) = (orient(a, b, c), orient(a, b, d), orient(c, d, a), orient(c, d, b));
    if ((o1 > 0.0 && o2 < 0.0) || (o1 < 0.0 && o2 > 0.0)) && ((o3 > 0.0 && o4 < 0.0) || (o3 < 0.0 && o4 > 0.0)) {
        return true;
    }
    on_seg(c, a, b) || on_seg(d, a, b) || on_seg(a, c, d) || on_seg(b, c, d)
}

/// Whether the segments cross at a point inside both of them.
fn segs_cross(a: P, b: P, c: P, d: P) -> bool {
    let (o1, o2, o3, o4) = (orient(a, b, c), orient(a, b, d), orient(c, d, a), orient(c, d, b));
    ((o1 > 0.0 && o2 < 0.0) || (o1 < 0.0 && o2 > 0.0)) && ((o3 > 0.0 && o4 < 0.0) || (o3 < 0.0 && o4 > 0.0))
}

fn seg_dist(p: P, a: P, b: P) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) };
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Loc {
    Out,
    Boundary,
    Inside,
}

/// The pieces of a geometry.
#[derive(Default)]
struct Parts {
    points: Vec<P>,
    lines: Vec<Vec<P>>,
    polys: Vec<Vec<Vec<P>>>,
}

impl Parts {
    fn of(g: &Geom) -> Parts {
        let mut p = Parts::default();
        p.add(g);
        p
    }
    fn add(&mut self, g: &Geom) {
        match g {
            Geom::Point(x) => self.points.push(*x),
            Geom::Line(l) if !l.is_empty() => self.lines.push(l.clone()),
            Geom::Polygon(r) if !r.is_empty() => self.polys.push(r.clone()),
            Geom::Multi(_, items) => items.iter().for_each(|i| self.add(i)),
            _ => {}
        }
    }
    fn segments(&self) -> Vec<(P, P)> {
        let mut out = Vec::new();
        for l in &self.lines {
            out.extend(l.windows(2).map(|w| (w[0], w[1])));
        }
        for poly in &self.polys {
            for r in poly {
                out.extend(r.windows(2).map(|w| (w[0], w[1])));
            }
        }
        out
    }
    fn dimension(&self) -> i32 {
        if !self.polys.is_empty() {
            2
        } else if !self.lines.is_empty() {
            1
        } else if !self.points.is_empty() {
            0
        } else {
            -1
        }
    }
    /// The points to test a relation with: the points, the vertices and the middles of the segments.
    fn samples(&self) -> Vec<P> {
        let mut out = self.points.clone();
        for l in &self.lines {
            out.extend(l.iter().copied());
        }
        for poly in &self.polys {
            for r in poly {
                out.extend(r.iter().copied());
            }
        }
        for (a, b) in self.segments() {
            out.push(((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0));
        }
        out
    }
    fn locate(&self, p: P) -> Loc {
        let mut best = Loc::Out;
        if self.points.contains(&p) {
            best = Loc::Inside;
        }
        for l in &self.lines {
            for w in l.windows(2) {
                if on_seg(p, w[0], w[1]) {
                    let closed = l.first() == l.last();
                    let end = !closed && (Some(&p) == l.first() || Some(&p) == l.last());
                    best = best.max(if end { Loc::Boundary } else { Loc::Inside });
                }
            }
        }
        for poly in &self.polys {
            let mut inside = false;
            let mut on_edge = false;
            for (k, r) in poly.iter().enumerate() {
                let mut inr = false;
                for w in r.windows(2) {
                    if on_seg(p, w[0], w[1]) {
                        on_edge = true;
                    }
                    let ((x1, y1), (x2, y2)) = (w[0], w[1]);
                    if (y1 > p.1) != (y2 > p.1) && p.0 < (x2 - x1) * (p.1 - y1) / (y2 - y1) + x1 {
                        inr = !inr;
                    }
                }
                if k == 0 {
                    inside = inr;
                } else if inr {
                    inside = false;
                }
            }
            best = best.max(if on_edge {
                Loc::Boundary
            } else if inside {
                Loc::Inside
            } else {
                Loc::Out
            });
        }
        best
    }
}

fn intersects(a: &Parts, b: &Parts) -> bool {
    if a.samples().iter().any(|p| b.locate(*p) != Loc::Out) || b.samples().iter().any(|p| a.locate(*p) != Loc::Out) {
        return true;
    }
    let sb = b.segments();
    a.segments().iter().any(|(p, q)| sb.iter().any(|(r, s)| segs_meet(*p, *q, *r, *s)))
}

fn interiors_meet(a: &Parts, b: &Parts) -> bool {
    if a.samples().iter().any(|p| a.locate(*p) == Loc::Inside && b.locate(*p) == Loc::Inside) || b.samples().iter().any(|p| a.locate(*p) == Loc::Inside && b.locate(*p) == Loc::Inside) {
        return true;
    }
    let sb = b.segments();
    a.segments().iter().any(|(p, q)| sb.iter().any(|(r, s)| segs_cross(*p, *q, *r, *s)))
}

/// Every point of `a` is in `b` (the border included).
fn covered(a: &Parts, b: &Parts) -> bool {
    a.samples().iter().all(|p| b.locate(*p) != Loc::Out)
}

fn within(a: &Parts, b: &Parts) -> bool {
    covered(a, b) && a.samples().iter().any(|p| b.locate(*p) == Loc::Inside && a.locate(*p) == Loc::Inside) && !(a.dimension() > b.dimension())
}

fn equal(a: &Parts, b: &Parts) -> bool {
    covered(a, b) && covered(b, a)
}

fn relation(name: &str, a: &Parts, b: &Parts) -> bool {
    match name {
        "CONTAINS" => within(b, a),
        "WITHIN" => within(a, b),
        "INTERSECTS" => intersects(a, b),
        "DISJOINT" => !intersects(a, b),
        "EQUALS" => equal(a, b),
        "TOUCHES" => intersects(a, b) && !interiors_meet(a, b) && !(a.dimension() == 0 && b.dimension() == 0),
        "OVERLAPS" => a.dimension() == b.dimension() && intersects(a, b) && interiors_meet(a, b) && !covered(a, b) && !covered(b, a),
        "CROSSES" => {
            let (da, db) = (a.dimension(), b.dimension());
            if da == 1 && db == 1 {
                let sb = b.segments();
                a.segments().iter().any(|(p, q)| sb.iter().any(|(r, s)| segs_cross(*p, *q, *r, *s)))
            } else if da < db {
                intersects(a, b) && interiors_meet(a, b) && !covered(a, b)
            } else {
                false
            }
        }
        _ => false,
    }
}

type Rect = (f64, f64, f64, f64);

fn mbr(g: &Geom) -> Option<Rect> {
    let parts = Parts::of(g);
    let all = parts.samples();
    let first = all.first()?;
    Some(all.iter().fold((first.0, first.1, first.0, first.1), |r, p| (r.0.min(p.0), r.1.min(p.1), r.2.max(p.0), r.3.max(p.1))))
}

fn mbr_relation(name: &str, a: Rect, b: Rect) -> bool {
    let meets = a.0 <= b.2 && b.0 <= a.2 && a.1 <= b.3 && b.1 <= a.3;
    let inside = |x: Rect, y: Rect| x.0 >= y.0 && x.1 >= y.1 && x.2 <= y.2 && x.3 <= y.3;
    let strictly = a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3;
    match name {
        "MBRCONTAINS" | "MBRCOVERS" => inside(b, a),
        "MBRWITHIN" | "MBRCOVEREDBY" => inside(a, b),
        "MBRINTERSECTS" => meets,
        "MBRDISJOINT" => !meets,
        "MBREQUALS" => a == b,
        "MBRTOUCHES" => meets && !strictly,
        "MBROVERLAPS" => strictly && !inside(a, b) && !inside(b, a),
        _ => false,
    }
}

fn ring_area(r: &[P]) -> f64 {
    r.windows(2).map(|w| w[0].0 * w[1].1 - w[1].0 * w[0].1).sum::<f64>().abs() / 2.0
}

fn area(g: &Geom) -> f64 {
    match g {
        Geom::Polygon(r) => r.iter().enumerate().map(|(k, x)| if k == 0 { ring_area(x) } else { -ring_area(x) }).sum(),
        Geom::Multi(_, items) => items.iter().map(area).sum(),
        _ => 0.0,
    }
}

fn length(g: &Geom) -> f64 {
    match g {
        Geom::Line(l) => l.windows(2).map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt()).sum(),
        Geom::Multi(_, items) => items.iter().map(length).sum(),
        _ => 0.0,
    }
}

fn distance(a: &Parts, b: &Parts) -> f64 {
    if intersects(a, b) {
        return 0.0;
    }
    let near = |x: &Parts, y: &Parts| {
        let sy = y.segments();
        let mut best = f64::INFINITY;
        for p in x.samples() {
            for q in &y.points {
                best = best.min(((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt());
            }
            for (s, t) in &sy {
                best = best.min(seg_dist(p, *s, *t));
            }
        }
        best
    };
    near(a, b).min(near(b, a))
}

fn count_points(g: &Geom) -> i64 {
    match g {
        Geom::Point(_) => 1,
        Geom::Line(l) => l.len() as i64,
        Geom::Polygon(r) => r.iter().map(|x| x.len() as i64).sum(),
        Geom::Multi(_, items) => items.iter().map(count_points).sum(),
    }
}

fn type_name(g: &Geom) -> &'static str {
    match g {
        Geom::Point(_) => "POINT",
        Geom::Line(_) => "LINESTRING",
        Geom::Polygon(_) => "POLYGON",
        Geom::Multi(Kind::Point, _) => "MULTIPOINT",
        Geom::Multi(Kind::Line, _) => "MULTILINESTRING",
        Geom::Multi(Kind::Polygon, _) => "MULTIPOLYGON",
        Geom::Multi(Kind::Collection, _) => "GEOMETRYCOLLECTION",
    }
}

fn is_empty(g: &Geom) -> bool {
    match g {
        Geom::Point(_) => false,
        Geom::Line(l) => l.is_empty(),
        Geom::Polygon(r) => r.is_empty(),
        Geom::Multi(_, items) => items.is_empty(),
    }
}

fn invalid(fname: &str) -> Error {
    Error::runtime(format!("Invalid GIS data provided to function {}.", fname.to_ascii_lowercase()))
}

/// Evaluates the function `name` (as registered in [`NAMES`]).
pub fn call(name: &str, vals: &[Cow<'_, Value>]) -> Result<Value> {
    let v = |i: usize| -> &Value { vals[i].as_ref() };
    let fname = name.to_ascii_lowercase();
    // the constructors of a type from text
    if let Some(stem) = name.strip_suffix("FROMTEXT") {
        let want = match stem.trim_start_matches("ST_") {
            "POINT" => Some("POINT"),
            "LINE" | "LINESTRING" => Some("LINESTRING"),
            "POLY" | "POLYGON" => Some("POLYGON"),
            "MPOINT" | "MULTIPOINT" => Some("MULTIPOINT"),
            "MLINE" | "MULTILINESTRING" => Some("MULTILINESTRING"),
            "MPOLY" | "MULTIPOLYGON" => Some("MULTIPOLYGON"),
            "GEOMCOLL" | "GEOMETRYCOLLECTION" => Some("GEOMETRYCOLLECTION"),
            _ => None,
        };
        let g = parse(&text_of(v(0))).ok_or_else(|| invalid(&fname))?;
        if want.is_some_and(|w| w != type_name(&g)) {
            return Err(invalid(&fname));
        }
        return Ok(out(&g));
    }
    if let Some(rest) = name.strip_prefix("MBR") {
        let _ = rest;
        let (Some(a), Some(b)) = (mbr(&geom_of(&fname, v(0))?), mbr(&geom_of(&fname, v(1))?)) else { return Ok(Value::Null) };
        return Ok(Value::Int(i64::from(mbr_relation(name, a, b))));
    }
    let base = name.strip_prefix("ST_").unwrap_or(name);
    if matches!(base, "CONTAINS" | "WITHIN" | "INTERSECTS" | "DISJOINT" | "EQUALS" | "TOUCHES" | "OVERLAPS" | "CROSSES") {
        let (a, b) = (Parts::of(&geom_of(&fname, v(0))?), Parts::of(&geom_of(&fname, v(1))?));
        return Ok(Value::Int(i64::from(relation(base, &a, &b))));
    }
    Ok(match base {
        "ASTEXT" | "ASWKT" => out(&geom_of(&fname, v(0))?),
        "POINT" => match (number(v(0)), number(v(1))) {
            (Some(x), Some(y)) => out(&Geom::Point((x, y))),
            _ => return Err(Error::runtime("Incorrect arguments to point")),
        },
        "LINESTRING" => {
            let mut p = Vec::new();
            for a in vals {
                match geom_of(&fname, a)? {
                    Geom::Point(x) => p.push(x),
                    _ => return Err(Error::runtime("Incorrect arguments to linestring")),
                }
            }
            let g = Geom::Line(p);
            if !valid(&g) {
                return Err(invalid(&fname));
            }
            out(&g)
        }
        "POLYGON" => {
            let mut r = Vec::new();
            for a in vals {
                match geom_of(&fname, a)? {
                    Geom::Line(l) => r.push(l),
                    _ => return Err(Error::runtime("Incorrect arguments to polygon")),
                }
            }
            let g = Geom::Polygon(r);
            if !valid(&g) {
                return Err(invalid(&fname));
            }
            out(&g)
        }
        "MULTIPOINT" | "MULTILINESTRING" | "MULTIPOLYGON" | "GEOMETRYCOLLECTION" => {
            let kind = match base {
                "MULTIPOINT" => Kind::Point,
                "MULTILINESTRING" => Kind::Line,
                "MULTIPOLYGON" => Kind::Polygon,
                _ => Kind::Collection,
            };
            let mut items = Vec::new();
            for a in vals {
                let g = geom_of(&fname, a)?;
                let ok = match kind {
                    Kind::Point => matches!(g, Geom::Point(_)),
                    Kind::Line => matches!(g, Geom::Line(_)),
                    Kind::Polygon => matches!(g, Geom::Polygon(_)),
                    Kind::Collection => true,
                };
                if !ok {
                    return Err(Error::runtime(format!("Incorrect arguments to {fname}")));
                }
                items.push(g);
            }
            out(&Geom::Multi(kind, items))
        }
        "X" | "Y" => match geom_of(&fname, v(0))? {
            Geom::Point(p) => match vals.get(1).and_then(|n| number(n)) {
                Some(n) => out(&Geom::Point(if base == "X" { (n, p.1) } else { (p.0, n) })),
                None => Value::Double(if base == "X" { p.0 } else { p.1 }),
            },
            _ => return Err(Error::runtime(format!("Incorrect type of geometry argument to {fname}"))),
        },
        "SRID" => {
            let g = geom_of(&fname, v(0))?;
            if vals.len() > 1 {
                out(&g)
            } else {
                Value::Int(0)
            }
        }
        "GEOMETRYTYPE" => Value::text(type_name(&geom_of(&fname, v(0))?)),
        "DIMENSION" => Value::Int(i64::from(Parts::of(&geom_of(&fname, v(0))?).dimension().max(0))),
        "NUMPOINTS" => match geom_of(&fname, v(0))? {
            g @ Geom::Line(_) => Value::Int(count_points(&g)),
            _ => return Err(Error::runtime(format!("Incorrect type of geometry argument to {fname}"))),
        },
        "NUMGEOMETRIES" => Value::Int(match geom_of(&fname, v(0))? {
            Geom::Multi(_, items) => items.len() as i64,
            g => i64::from(!is_empty(&g)),
        }),
        "NUMINTERIORRINGS" | "NUMINTERIORRING" => match geom_of(&fname, v(0))? {
            Geom::Polygon(r) => Value::Int(r.len().saturating_sub(1) as i64),
            _ => return Err(Error::runtime(format!("Incorrect type of geometry argument to {fname}"))),
        },
        "GEOMETRYN" => {
            let n = number(v(1)).unwrap_or(0.0) as i64;
            match geom_of(&fname, v(0))? {
                Geom::Multi(_, items) if n >= 1 && (n as usize) <= items.len() => out(&items[n as usize - 1]),
                g if n == 1 && !is_empty(&g) => out(&g),
                _ => Value::Null,
            }
        }
        "POINTN" => {
            let n = number(v(1)).unwrap_or(0.0) as i64;
            match geom_of(&fname, v(0))? {
                Geom::Line(l) if n >= 1 && (n as usize) <= l.len() => out(&Geom::Point(l[n as usize - 1])),
                Geom::Line(_) => Value::Null,
                _ => return Err(Error::runtime(format!("Incorrect type of geometry argument to {fname}"))),
            }
        }
        "STARTPOINT" | "ENDPOINT" => match geom_of(&fname, v(0))? {
            Geom::Line(l) if !l.is_empty() => out(&Geom::Point(if base == "STARTPOINT" { l[0] } else { l[l.len() - 1] })),
            Geom::Line(_) => Value::Null,
            _ => return Err(Error::runtime(format!("Incorrect type of geometry argument to {fname}"))),
        },
        "EXTERIORRING" => match geom_of(&fname, v(0))? {
            Geom::Polygon(r) if !r.is_empty() => out(&Geom::Line(r[0].clone())),
            Geom::Polygon(_) => Value::Null,
            _ => return Err(Error::runtime(format!("Incorrect type of geometry argument to {fname}"))),
        },
        "INTERIORRINGN" => {
            let n = number(v(1)).unwrap_or(0.0) as i64;
            match geom_of(&fname, v(0))? {
                Geom::Polygon(r) if n >= 1 && (n as usize) < r.len() => out(&Geom::Line(r[n as usize].clone())),
                Geom::Polygon(_) => Value::Null,
                _ => return Err(Error::runtime(format!("Incorrect type of geometry argument to {fname}"))),
            }
        }
        "ISEMPTY" => Value::Int(i64::from(is_empty(&geom_of(&fname, v(0))?))),
        "ISCLOSED" => {
            fn closed(g: &Geom) -> bool {
                match g {
                    Geom::Line(l) => !l.is_empty() && l.first() == l.last(),
                    Geom::Multi(_, items) => !items.is_empty() && items.iter().all(closed),
                    _ => false,
                }
            }
            match geom_of(&fname, v(0))? {
                g @ (Geom::Line(_) | Geom::Multi(Kind::Line, _)) => Value::Int(i64::from(closed(&g))),
                _ => return Err(Error::runtime(format!("Incorrect type of geometry argument to {fname}"))),
            }
        }
        "ISSIMPLE" => {
            let g = geom_of(&fname, v(0))?;
            let lines: Vec<Vec<P>> = Parts::of(&g).lines;
            let mut simple = true;
            for l in &lines {
                let segs: Vec<(P, P)> = l.windows(2).map(|w| (w[0], w[1])).collect();
                for i in 0..segs.len() {
                    for j in i + 2..segs.len() {
                        let adjacent_closed = i == 0 && j == segs.len() - 1 && l.first() == l.last();
                        if !adjacent_closed && segs_meet(segs[i].0, segs[i].1, segs[j].0, segs[j].1) {
                            simple = false;
                        }
                    }
                }
            }
            Value::Int(i64::from(simple))
        }
        "ISVALID" => {
            geom_of(&fname, v(0))?;
            Value::Int(1)
        }
        "LENGTH" | "GLENGTH" => Value::Double(length(&geom_of(&fname, v(0))?)),
        "AREA" => Value::Double(area(&geom_of(&fname, v(0))?)),
        "ENVELOPE" => {
            let g = geom_of(&fname, v(0))?;
            match mbr(&g) {
                Some((x1, y1, x2, y2)) if (x1, y1) == (x2, y2) => out(&Geom::Point((x1, y1))),
                Some((x1, y1, x2, y2)) => out(&Geom::Polygon(vec![vec![(x1, y1), (x2, y1), (x2, y2), (x1, y2), (x1, y1)]])),
                None => Value::Null,
            }
        }
        "CENTROID" => {
            let g = geom_of(&fname, v(0))?;
            let parts = Parts::of(&g);
            let all: Vec<P> = match parts.dimension() {
                2 => parts.polys.iter().flat_map(|p| p[0].iter().take(p[0].len() - 1).copied()).collect(),
                1 => parts.lines.iter().flatten().copied().collect(),
                _ => parts.points.clone(),
            };
            if all.is_empty() {
                Value::Null
            } else {
                let n = all.len() as f64;
                out(&Geom::Point((all.iter().map(|p| p.0).sum::<f64>() / n, all.iter().map(|p| p.1).sum::<f64>() / n)))
            }
        }
        "DISTANCE" => {
            let (a, b) = (Parts::of(&geom_of(&fname, v(0))?), Parts::of(&geom_of(&fname, v(1))?));
            let d = distance(&a, &b);
            if d.is_finite() {
                Value::Double(d)
            } else {
                Value::Null
            }
        }
        _ => return Err(Error::runtime(format!("unknown function '{name}'"))),
    })
}
