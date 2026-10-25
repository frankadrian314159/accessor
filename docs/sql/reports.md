# Reports

A *report* turns a query into a document: rows **grouped** under headings, **totals** at the end of each group and of the whole, columns **formatted** and aligned, and long output **paged**. It is stored in the database like a saved query, and can be rendered as plain text, HTML, Markdown, CSV or PDF.

```sql
CREATE REPORT SalesByRegion
    TITLE 'Sales by region'
    SUBTITLE 'All figures in dollars'
    BREAK BY Region HEADER 'Region {Region}' FOOTER 'Total {Region} ({count} sales)'
    BREAK BY City
    COLUMN Rep HEADING 'Representative'
    COLUMN Amount FORMAT '#,##0.00' SUM
    COLUMN Units SUM
    GRAND TOTAL 'All regions'
AS SELECT Region, City, Rep, Amount, Units FROM Sales;

RUN REPORT SalesByRegion;                 -- one row, one column: the report as text
RUN REPORT SalesByRegion AS HTML;
```

gives

```
Sales by region
All figures in dollars

Representative    Amount  Units
-------------------------------
Region North
  City: Bergen
    Cy                 99.99      2
                    --------  -----
    Total Bergen       99.99      2
  City: Oslo
    Ada             1,200.50      3
    Bo                300.00      1
                    --------  -----
    Total Oslo      1,500.50      4
                    --------  -----
  Total North (3 sales)
                    1,600.49      6
Region South
  City: Milan
    Flo                75.00      1
                    --------  -----
    Total Milan        75.00      1
  City: Rome
    Di              4,000.00     10
    Ed                250.25      5
                    --------  -----
    Total Rome      4,250.25     15
                    --------  -----
  Total South (3 sales)
                    4,325.25     16
                    --------  -----
    All regions     5,925.74     22
Page 1 of 1
```

## CREATE REPORT

```sql
CREATE [OR REPLACE] REPORT name
    [TITLE 'text'] [SUBTITLE 'text']
    [BREAK [BY] column [HEADER 'text'] [FOOTER 'text'] [NEW PAGE]] ...
    [COLUMN column [HEADING 'text'] [FORMAT 'pattern'] [ALIGN LEFT|RIGHT|CENTER] [WIDTH n]
                    [SUM|AVG|COUNT|MIN|MAX] [HIDE]] ...
    [GRAND TOTAL ['label']]
    [PAGE SIZE n [LINES]]
AS select;

DROP REPORT [IF EXISTS] name;
```

The layout comes first and the query last, so that the query can be any `SELECT`. All the layout clauses are optional; a report with none is the query's rows as a table.

| Clause | Meaning |
| --- | --- |
| `TITLE`, `SUBTITLE` | the heading of the report (the title defaults to the report's name) |
| `BREAK BY col` | start a new group when the value of `col` changes. Several `BREAK`s nest, outermost first. The break column is shown in the group's heading, not repeated in every row, unless a `COLUMN` clause names it |
| `HEADER 'text'` | the group's heading. `{col}` is replaced by that column's value in the group's first row. The default is `col: value` |
| `FOOTER 'text'` | the label of the line of totals that closes the group. `{col}` is the group's value and `{count}` the number of rows. The default is `Total value`. A group gets a closing line when it has a footer or the report has any total column |
| `NEW PAGE` | each group of this level starts a new page (text and PDF; in HTML, a print page break) |
| `COLUMN col` | how to show a column of the query |
| `HEADING 'text'` | the column's heading (default: its name) |
| `FORMAT 'pattern'` | a [`Format`](../functions/format.md) pattern for the values and the totals: `'#,##0.00'`, `'dd mmm yyyy'`, ... |
| `ALIGN` | left, right or centre; numbers default to right |
| `WIDTH n` | the width in the text and PDF forms (default: the widest value, at most 60; longer text is cut with `…`) |
| `SUM`, `AVG`, `COUNT`, `MIN`, `MAX` | total the column in each group's closing line and in the grand total, with the exact arithmetic of the SQL aggregates (money stays exact) |
| `HIDE` | leave the column out |
| `GRAND TOTAL ['label']` | a closing line for the whole report (default label `Grand total`) |
| `PAGE SIZE n` | lines per page in the text and PDF forms (default 60) |

The report is checked when it is created: the query must run, and every column and `{placeholder}` in the layout must be one the query returns.

## Order

Groups are runs of equal values, so the rows must arrive sorted by the break columns. If the query has no `ORDER BY`, the report adds one on the break columns, which the [optimizer](../optimizer/access-paths.md) can answer from an index. If you write your own `ORDER BY`, it is used as written, and a break column that is out of order simply starts a new group each time its value changes.

Reports total for themselves, so a `GROUP BY ROLLUP` query is not needed for subtotals; a report over an already-summarised query (`SELECT Region, SUM(Amount) ... GROUP BY Region`) works too.

## Rendering

| Form | How |
| --- | --- |
| text in a result | `RUN REPORT name [AS TEXT \| HTML \| MARKDOWN \| CSV]` returns one row with one column, `report` |
| any form, as bytes | `db.render_report("name", ReportFormat::Pdf)` in Rust |
| a file from the shell | `.report name file.pdf` (the extension picks the form: `.txt` `.html` `.md` `.csv` `.pdf`); `.report name` prints the text; `.reports` lists them |

* **Text**: fixed-width columns, indented groups, a rule above each line of totals, a repeated heading on every page and `Page n of m` at the foot of each. A group heading is never left alone at the foot of a page. Pages are separated by a form feed.
* **HTML**: one self-contained file (no scripts, no external styles); headings repeat on every printed page and groups may start new pages. Print it to PDF from a browser for a PDF with proportional fonts.
* **Markdown**: a pipe table with group headings and totals in bold.
* **CSV**: the detail rows and the totals, with the label of a total in the first cell; group headings are left out.
* **PDF**: the text form on pages of monospaced type (Courier, 9 point), A4 portrait, or landscape if the lines are wide. It uses the standard font, so characters outside Latin-1 are shown as `?`.

## Storage

A report is stored as its `CREATE REPORT` text, in the same file as the tables, and creating or dropping one is transactional. `Database::report_names()` lists them.
