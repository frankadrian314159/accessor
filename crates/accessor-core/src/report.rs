//! Reports: grouped, totalled and paged output of a query, as text, HTML, Markdown, CSV or PDF.
//!
//! A report is a query plus a layout (see [`ReportLayout`]): which columns to break on, what to put in
//! each group's heading and footing, which columns to total, how to format them, how long a page is.
//! The query is run by the ordinary engine; this module only turns its rows into a document. Totals
//! are computed with the same exact arithmetic as `SUM` and `AVG` in a query.

use crate::error::{Error, Result};
use crate::exec::agg::{AggFn, AggSpec, AggState};
use crate::exec::QueryResult;
use crate::expr::format::format_value;
use crate::sql::ast::{ReportAlign, ReportColumn, ReportLayout, ReportTotal};
use crate::value::Value;

/// The kinds of document a report can be rendered as.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReportFormat {
    Text,
    Html,
    Markdown,
    Csv,
    Pdf,
}

impl ReportFormat {
    /// `TEXT`, `HTML`, `MARKDOWN` (or `MD`), `CSV`, `PDF`; case does not matter.
    pub fn parse(name: &str) -> Option<ReportFormat> {
        Some(match name.to_ascii_lowercase().as_str() {
            "text" | "txt" => ReportFormat::Text,
            "html" | "htm" => ReportFormat::Html,
            "markdown" | "md" => ReportFormat::Markdown,
            "csv" => ReportFormat::Csv,
            "pdf" => ReportFormat::Pdf,
            _ => return None,
        })
    }

    /// The format a file name asks for, by its extension.
    pub fn from_path(path: &str) -> Option<ReportFormat> {
        ReportFormat::parse(path.rsplit_once('.')?.1)
    }
}

/// Lines on a page unless the layout says otherwise.
const DEFAULT_PAGE_LINES: usize = 60;
/// The widest a column is made; longer text is cut.
const MAX_WIDTH: usize = 60;

struct Col {
    /// Position in the query's result.
    at: usize,
    heading: String,
    right: bool,
    format: Option<String>,
    total: Option<ReportTotal>,
    width: Option<usize>,
}

enum Item {
    Group { level: usize, text: String, new_page: bool },
    Detail(Vec<String>),
    /// A line of totals: `level` is `None` for the grand total.
    Total { level: Option<usize>, label: String, cells: Vec<Option<String>> },
}

fn total_fn(t: ReportTotal) -> AggFn {
    match t {
        ReportTotal::Sum => AggFn::Sum,
        ReportTotal::Avg => AggFn::Avg,
        ReportTotal::Count => AggFn::Count,
        ReportTotal::Min => AggFn::Min,
        ReportTotal::Max => AggFn::Max,
    }
}

fn spec_of(t: ReportTotal) -> AggSpec {
    AggSpec { func: total_fn(t), arg: None, distinct: false, sep: None, order: Vec::new() }
}

/// Checks a layout against the columns the query returns.
pub(crate) fn validate(columns: &[String], layout: &ReportLayout) -> Result<()> {
    let find = |n: &str| {
        columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(n))
            .ok_or_else(|| Error::catalog(format!("the report refers to the column '{n}', which its query does not return")))
    };
    for b in &layout.breaks {
        find(&b.column)?;
        for t in [&b.header, &b.footer].into_iter().flatten() {
            expand(t, |n| find(n).map(|_| String::new()), 0)?;
        }
    }
    for c in &layout.columns {
        find(&c.name)?;
    }
    Ok(())
}

/// Replaces `{name}` in `template` using `value`; `{count}` is the number of rows.
fn expand(template: &str, value: impl Fn(&str) -> Result<String>, count: usize) -> Result<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let name = rest[open + 1..open + close].trim();
        if name.eq_ignore_ascii_case("count") {
            out.push_str(&count.to_string());
        } else {
            out.push_str(&value(name)?);
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn cut(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(width.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Turns the rows of a query into a document.
pub(crate) fn render(result: &QueryResult, name: &str, layout: &ReportLayout, format: ReportFormat) -> Result<Vec<u8>> {
    validate(&result.columns, layout)?;
    let at = |n: &str| result.columns.iter().position(|c| c.eq_ignore_ascii_case(n)).expect("validated");
    let breaks: Vec<usize> = layout.breaks.iter().map(|b| at(&b.column)).collect();

    // The columns shown: all of the query's, except the break columns (their value is in the group
    // heading) unless the layout names them, and except those it hides.
    let mut cols: Vec<Col> = Vec::new();
    for (i, cname) in result.columns.iter().enumerate() {
        let spec: Option<&ReportColumn> = layout.columns.iter().find(|c| c.name.eq_ignore_ascii_case(cname));
        if spec.is_some_and(|s| s.hidden) || (spec.is_none() && breaks.contains(&i)) {
            continue;
        }
        let numeric = result.rows.iter().find_map(|r| (!r[i].is_null()).then(|| r[i].is_numeric()));
        cols.push(Col {
            at: i,
            heading: spec.and_then(|s| s.heading.clone()).unwrap_or_else(|| cname.clone()),
            right: match spec.and_then(|s| s.align) {
                Some(ReportAlign::Right) => true,
                Some(_) => false,
                None => numeric == Some(true),
            },
            format: spec.and_then(|s| s.format.clone()),
            total: spec.and_then(|s| s.total),
            width: spec.and_then(|s| s.width),
        });
    }
    let centre: Vec<bool> = cols
        .iter()
        .map(|c| layout.columns.iter().any(|s| s.name.eq_ignore_ascii_case(&result.columns[c.at]) && s.align == Some(ReportAlign::Center)))
        .collect();

    let show = |v: &Value, format: &Option<String>| -> Result<String> {
        if v.is_null() {
            return Ok(String::new());
        }
        match format {
            Some(p) => format_value(v, p),
            None => Ok(v.to_string()),
        }
    };
    let plain = |v: &Value| if v.is_null() { String::new() } else { v.to_string() };

    // Walk the rows, opening and closing groups where the break values change.
    struct Acc {
        states: Vec<Option<AggState>>,
        rows: usize,
        key: Vec<Value>,
    }
    let fresh = |cols: &[Col]| -> Vec<Option<AggState>> { cols.iter().map(|c| c.total.map(|t| AggState::new(&spec_of(t)))).collect() };
    let mut items: Vec<Item> = Vec::new();
    let mut open: Vec<Acc> = Vec::new();
    let mut grand = fresh(&cols);
    let any_total = cols.iter().any(|c| c.total.is_some());

    let close = |level: usize, acc: Acc, items: &mut Vec<Item>| -> Result<()> {
        let b = &layout.breaks[level];
        if b.footer.is_none() && !any_total {
            return Ok(());
        }
        let value_of = |n: &str| -> Result<String> {
            let i = at(n);
            Ok(breaks.iter().position(|&x| x == i).map_or_else(|| String::new(), |l| plain(&acc.key[l])))
        };
        let label = match &b.footer {
            Some(t) => expand(t, value_of, acc.rows)?,
            None => format!("Total {}", acc.key.get(level).map(plain).unwrap_or_default()),
        };
        let cells = acc
            .states
            .into_iter()
            .zip(&cols)
            .map(|(s, c)| s.map(|s| finish_total(s.finish(), c, &show)).transpose())
            .collect::<Result<Vec<_>>>()?;
        items.push(Item::Total { level: Some(level), label, cells });
        Ok(())
    };

    for row in &result.rows {
        let key: Vec<Value> = breaks.iter().map(|&i| row[i].clone()).collect();
        // The outermost level whose value changed (all of them, for the first row).
        let changed = (0..breaks.len()).find(|&l| open.get(l).is_none_or(|a| a.key[l] != key[l]));
        if let Some(level) = changed {
            while open.len() > level {
                let acc = open.pop().expect("non-empty");
                close(open.len(), acc, &mut items)?;
            }
            for l in level..breaks.len() {
                let b = &layout.breaks[l];
                let value_of = |n: &str| -> Result<String> {
                    let i = at(n);
                    Ok(show(&row[i], &layout.columns.iter().find(|c| c.name.eq_ignore_ascii_case(n)).and_then(|c| c.format.clone()))?)
                };
                let text = match &b.header {
                    Some(t) => expand(t, value_of, 0)?,
                    None => format!("{}: {}", result.columns[breaks[l]], plain(&row[breaks[l]])),
                };
                items.push(Item::Group { level: l, text, new_page: b.new_page && (l > 0 || !items.is_empty()) });
                // The whole key is kept, so a footer can name the outer groups too.
                open.push(Acc { states: fresh(&cols), rows: 0, key: key.clone() });
            }
        }
        for acc in open.iter_mut() {
            acc.rows += 1;
            for (s, c) in acc.states.iter_mut().zip(&cols) {
                if let Some(s) = s {
                    s.update(Some(&row[c.at]))?;
                }
            }
        }
        for (s, c) in grand.iter_mut().zip(&cols) {
            if let Some(s) = s {
                s.update(Some(&row[c.at]))?;
            }
        }
        let mut cells = Vec::with_capacity(cols.len());
        for c in &cols {
            cells.push(show(&row[c.at], &c.format)?);
        }
        items.push(Item::Detail(cells));
    }
    while let Some(acc) = open.pop() {
        close(open.len(), acc, &mut items)?;
    }
    if let Some(label) = &layout.grand_total {
        if any_total {
            let cells = grand
                .into_iter()
                .zip(&cols)
                .map(|(s, c)| s.map(|s| finish_total(s.finish(), c, &show)).transpose())
                .collect::<Result<Vec<_>>>()?;
            items.push(Item::Total { level: None, label: label.clone(), cells });
        }
    }

    let title = layout.title.clone().unwrap_or_else(|| name.to_string());
    let headings: Vec<&str> = cols.iter().map(|c| c.heading.as_str()).collect();
    let right: Vec<bool> = cols.iter().map(|c| c.right).collect();
    let nbreaks = breaks.len();
    let page_lines = layout.page_lines.unwrap_or(DEFAULT_PAGE_LINES).max(8);
    let widths = widths_of(&cols, &headings, &items);

    Ok(match format {
        ReportFormat::Text => paginate(&title, layout.subtitle.as_deref(), &headings, &widths, &right, &centre, &items, nbreaks, page_lines)
            .iter()
            .map(|p| p.join("\n"))
            .collect::<Vec<_>>()
            .join("\n\u{c}\n")
            .into_bytes(),
        ReportFormat::Pdf => {
            let pages = paginate(&title, layout.subtitle.as_deref(), &headings, &widths, &right, &centre, &items, nbreaks, page_lines);
            pdf(&pages)
        }
        ReportFormat::Html => html(&title, layout.subtitle.as_deref(), &headings, &right, &items).into_bytes(),
        ReportFormat::Markdown => markdown(&title, layout.subtitle.as_deref(), &headings, &right, &items).into_bytes(),
        ReportFormat::Csv => csv(&headings, &items).into_bytes(),
    })
}

fn finish_total(v: Value, c: &Col, show: &impl Fn(&Value, &Option<String>) -> Result<String>) -> Result<String> {
    // A count is a count whatever the column's pattern; the others take its pattern.
    if c.total == Some(ReportTotal::Count) {
        return Ok(v.to_string());
    }
    show(&v, &c.format)
}

fn widths_of(cols: &[Col], headings: &[&str], items: &[Item]) -> Vec<usize> {
    let mut w: Vec<usize> = headings.iter().map(|h| h.chars().count()).collect();
    for item in items {
        match item {
            Item::Detail(cells) => {
                for (i, c) in cells.iter().enumerate() {
                    w[i] = w[i].max(c.chars().count());
                }
            }
            Item::Total { cells, .. } => {
                for (i, c) in cells.iter().enumerate() {
                    if let Some(c) = c {
                        w[i] = w[i].max(c.chars().count());
                    }
                }
            }
            Item::Group { .. } => {}
        }
    }
    w.iter().zip(cols).map(|(&n, c)| c.width.unwrap_or(n.min(MAX_WIDTH)).max(1)).collect()
}

fn pad(s: &str, width: usize, right: bool, centre: bool) -> String {
    let s = cut(s, width);
    let fill = width.saturating_sub(s.chars().count());
    if centre {
        format!("{}{}{}", " ".repeat(fill / 2), s, " ".repeat(fill - fill / 2))
    } else if right {
        format!("{}{}", " ".repeat(fill), s)
    } else {
        format!("{}{}", s, " ".repeat(fill))
    }
}

/// The pages of a fixed-width rendering.
#[allow(clippy::too_many_arguments)]
fn paginate(
    title: &str,
    subtitle: Option<&str>,
    headings: &[&str],
    widths: &[usize],
    right: &[bool],
    centre: &[bool],
    items: &[Item],
    nbreaks: usize,
    page_lines: usize,
) -> Vec<Vec<String>> {
    let join = |cells: Vec<String>| cells.join("  ").trim_end().to_string();
    let line_width: usize = widths.iter().sum::<usize>() + 2 * widths.len().saturating_sub(1);
    let mut head: Vec<String> = Vec::new();
    head.push(title.to_string());
    if let Some(s) = subtitle {
        head.push(s.to_string());
    }
    head.push(String::new());
    head.push(join(headings.iter().enumerate().map(|(i, h)| pad(h, widths[i], right[i], centre[i])).collect()));
    head.push("-".repeat(line_width.max(title.chars().count())));
    let indent = |level: usize| " ".repeat(2 * level);

    // Body lines, each with whether it must begin a page and whether it is a group heading.
    let mut body: Vec<(String, bool, bool)> = Vec::new();
    for item in items {
        match item {
            Item::Group { level, text, new_page } => body.push((format!("{}{}", indent(*level), text), *new_page, true)),
            Item::Detail(cells) => {
                let line = join(cells.iter().enumerate().map(|(i, c)| pad(c, widths[i], right[i], centre[i])).collect());
                body.push((format!("{}{}", indent(nbreaks), line), false, false));
            }
            Item::Total { level, label, cells } => {
                let lvl = level.map_or(0, |l| l + 1);
                let rule = join(cells.iter().enumerate().map(|(i, c)| if c.is_some() { "-".repeat(widths[i]) } else { " ".repeat(widths[i]) }).collect());
                body.push((format!("{}{}", indent(nbreaks), rule), false, false));
                let first_free = cells.first().is_none_or(|c| c.is_none());
                let mut line_cells: Vec<String> = cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| pad(c.as_deref().unwrap_or(""), widths[i], right[i], false))
                    .collect();
                if first_free && !line_cells.is_empty() && label.chars().count() <= widths[0] {
                    line_cells[0] = pad(label, widths[0], false, false);
                    body.push((format!("{}{}", indent(nbreaks), join(line_cells)), false, false));
                } else {
                    body.push((format!("{}{}", indent(lvl.min(nbreaks)), label), false, false));
                    body.push((format!("{}{}", indent(nbreaks), join(line_cells)), false, false));
                }
            }
        }
    }

    let capacity = page_lines.saturating_sub(head.len() + 2).max(3);
    let mut pages: Vec<Vec<String>> = vec![Vec::new()];
    for (line, new_page, is_group) in &body {
        let used = pages.last().expect("one page").len();
        // A group heading is not left alone at the foot of a page.
        let orphan = *is_group && used + 2 > capacity && used > 0;
        let is_first_line = used == 0;
        if (*new_page && !is_first_line) || used >= capacity || orphan {
            pages.push(Vec::new());
        }
        pages.last_mut().expect("one page").push(line.clone());
    }
    let n = pages.len();
    pages
        .into_iter()
        .enumerate()
        .map(|(p, lines)| {
            let mut page = head.clone();
            page.extend(lines);
            // Pad so the page number sits at the foot.
            while page.len() < page_lines.saturating_sub(1) && n > 1 {
                page.push(String::new());
            }
            page.push(format!("Page {} of {}", p + 1, n));
            page
        })
        .collect()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn html(title: &str, subtitle: Option<&str>, headings: &[&str], right: &[bool], items: &[Item]) -> String {
    let n = headings.len().max(1);
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str(&format!("<title>{}</title>\n", esc(title)));
    out.push_str(
        "<style>\n\
         body { font-family: system-ui, sans-serif; margin: 2rem; color: #1a1a1a; }\n\
         h1 { margin: 0 0 .25rem; font-size: 1.5rem; } p.sub { margin: 0 0 1rem; color: #555; }\n\
         table { border-collapse: collapse; width: 100%; font-size: .9rem; }\n\
         th { text-align: left; border-bottom: 2px solid #333; padding: .3rem .6rem; }\n\
         td { padding: .2rem .6rem; } td.r, th.r { text-align: right; font-variant-numeric: tabular-nums; }\n\
         tr.group td { font-weight: 600; padding-top: .8rem; border-bottom: 1px solid #bbb; }\n\
         tr.g1 td { padding-left: 1.5rem; } tr.g2 td { padding-left: 2.5rem; } tr.g3 td { padding-left: 3.5rem; }\n\
         tr.total td { font-weight: 600; border-top: 1px solid #333; }\n\
         tr.grand td { border-top: 3px double #333; }\n\
         @media print { body { margin: 0; } thead { display: table-header-group; } tr { break-inside: avoid; } tbody.newpage { break-before: page; } }\n\
         </style>\n</head>\n<body>\n",
    );
    out.push_str(&format!("<h1>{}</h1>\n", esc(title)));
    if let Some(s) = subtitle {
        out.push_str(&format!("<p class=\"sub\">{}</p>\n", esc(s)));
    }
    out.push_str("<table>\n<thead><tr>");
    for (i, h) in headings.iter().enumerate() {
        out.push_str(&format!("<th{}>{}</th>", if right[i] { " class=\"r\"" } else { "" }, esc(h)));
    }
    out.push_str("</tr></thead>\n<tbody>\n");
    for item in items {
        match item {
            Item::Group { level, text, new_page } => {
                if *new_page {
                    out.push_str("</tbody>\n<tbody class=\"newpage\">\n");
                }
                out.push_str(&format!("<tr class=\"group g{level}\"><td colspan=\"{n}\">{}</td></tr>\n", esc(text)));
            }
            Item::Detail(cells) => {
                out.push_str("<tr>");
                for (i, c) in cells.iter().enumerate() {
                    out.push_str(&format!("<td{}>{}</td>", if right[i] { " class=\"r\"" } else { "" }, esc(c)));
                }
                out.push_str("</tr>\n");
            }
            Item::Total { level, label, cells } => {
                out.push_str(&format!("<tr class=\"total{}\">", if level.is_none() { " grand" } else { "" }));
                let first_free = cells.first().is_none_or(|c| c.is_none());
                for (i, c) in cells.iter().enumerate() {
                    let text = match c {
                        Some(t) => t.as_str(),
                        None if i == 0 && first_free => label.as_str(),
                        None => "",
                    };
                    out.push_str(&format!("<td{}>{}</td>", if right[i] && c.is_some() { " class=\"r\"" } else { "" }, esc(text)));
                }
                out.push_str("</tr>\n");
                if !first_free {
                    // The label has no cell of its own; give it a line.
                    out.push_str(&format!("<!-- {} -->\n", esc(label)));
                }
            }
        }
    }
    out.push_str("</tbody>\n</table>\n</body>\n</html>\n");
    out
}

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|")
}

fn markdown(title: &str, subtitle: Option<&str>, headings: &[&str], right: &[bool], items: &[Item]) -> String {
    let mut out = format!("# {title}\n\n");
    if let Some(s) = subtitle {
        out.push_str(&format!("{s}\n\n"));
    }
    out.push_str(&format!("| {} |\n", headings.iter().map(|h| md_cell(h)).collect::<Vec<_>>().join(" | ")));
    out.push_str(&format!("|{}|\n", right.iter().map(|r| if *r { " ---: " } else { " --- " }).collect::<Vec<_>>().join("|")));
    let n = headings.len();
    for item in items {
        match item {
            Item::Group { text, .. } => {
                let mut cells = vec![String::new(); n];
                cells[0] = format!("**{}**", md_cell(text));
                out.push_str(&format!("| {} |\n", cells.join(" | ")));
            }
            Item::Detail(cells) => out.push_str(&format!("| {} |\n", cells.iter().map(|c| md_cell(c)).collect::<Vec<_>>().join(" | "))),
            Item::Total { label, cells, .. } => {
                let first_free = cells.first().is_none_or(|c| c.is_none());
                let row: Vec<String> = cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| match c {
                        Some(t) => format!("**{}**", md_cell(t)),
                        None if i == 0 && first_free => format!("**{}**", md_cell(label)),
                        None => String::new(),
                    })
                    .collect();
                out.push_str(&format!("| {} |\n", row.join(" | ")));
            }
        }
    }
    out
}

fn csv_cell(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn csv(headings: &[&str], items: &[Item]) -> String {
    let mut out = format!("{}\n", headings.iter().map(|h| csv_cell(h)).collect::<Vec<_>>().join(","));
    for item in items {
        match item {
            Item::Group { .. } => {}
            Item::Detail(cells) => out.push_str(&format!("{}\n", cells.iter().map(|c| csv_cell(c)).collect::<Vec<_>>().join(","))),
            Item::Total { label, cells, .. } => {
                let first_free = cells.first().is_none_or(|c| c.is_none());
                let row: Vec<String> = cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| match c {
                        Some(t) => csv_cell(t),
                        None if i == 0 && first_free => csv_cell(label),
                        None => String::new(),
                    })
                    .collect();
                out.push_str(&format!("{}\n", row.join(",")));
            }
        }
    }
    out
}

/// A plain PDF of monospaced pages.
fn pdf(pages: &[Vec<String>]) -> Vec<u8> {
    const FONT: f32 = 9.0;
    const LEAD: f32 = 11.0;
    const CHAR: f32 = 5.4; // Courier at 9 points
    const MARGIN: f32 = 36.0;
    let widest = pages.iter().flatten().map(|l| l.chars().count()).max().unwrap_or(0) as f32;
    let (w, h) = if widest * CHAR + 2.0 * MARGIN > 595.0 { (842.0, 595.0) } else { (595.0, 842.0) };

    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let mut object = |out: &mut Vec<u8>, body: &[u8]| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    let n = pages.len().max(1);
    // 1 catalog, 2 page tree, 3 font, then a page and its content for each page.
    object(&mut out, b"<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    object(&mut out, format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).as_bytes());
    object(&mut out, b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>");
    let empty: Vec<String> = Vec::new();
    for i in 0..n {
        let lines = pages.get(i).unwrap_or(&empty);
        let mut content = format!("BT /F1 {FONT} Tf {LEAD} TL {MARGIN} {} Td\n", h - MARGIN - FONT).into_bytes();
        for line in lines {
            content.push(b'(');
            for ch in line.chars() {
                match ch {
                    '(' | ')' | '\\' => {
                        content.push(b'\\');
                        content.push(ch as u8);
                    }
                    c if (' '..='~').contains(&c) => content.push(c as u8),
                    c if (c as u32) >= 0xA0 && (c as u32) <= 0xFF => content.push(c as u32 as u8),
                    '\u{2026}' => content.push(0x85),
                    _ => content.push(b'?'),
                }
            }
            content.extend_from_slice(b") Tj T*\n");
        }
        content.extend_from_slice(b"ET");
        object(
            &mut out,
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>", 5 + 2 * i).as_bytes(),
        );
        let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        stream.extend_from_slice(&content);
        stream.extend_from_slice(b"\nendstream");
        object(&mut out, &stream);
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes());
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len() + 1).as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates() {
        let r = expand("Total for {Region} ({count} rows)", |n| Ok(n.to_uppercase()), 3).unwrap();
        assert_eq!(r, "Total for REGION (3 rows)");
        assert_eq!(expand("no braces", |_| Ok(String::new()), 0).unwrap(), "no braces");
    }

    #[test]
    fn a_pdf_is_a_pdf() {
        let bytes = pdf(&[vec!["hello (world)".to_string()], vec!["page two".to_string()]]);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.contains("/Count 2"));
        assert!(text.contains("hello \\(world\\)"));
        assert!(text.trim_end().ends_with("%%EOF"));
    }
}
