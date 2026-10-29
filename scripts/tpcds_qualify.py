#!/usr/bin/env python3
"""Writes a copy of the TPC-DS query templates with the qualification substitution parameters of the specification
(Appendix B) in place of the random ones. `dsqgen -qualify Y` only puts the queries in order; it does not use the
qualification values, so its queries cannot give the answer sets.

  tpcds_qualify.py SPEC.pdf TEMPLATES_DIR OUT_DIR

OUT_DIR is a copy of TEMPLATES_DIR in which `[NAME]` and `[NAME.k]` are replaced by their qualification values. Run
`dsqgen -directory OUT_DIR -input OUT_DIR/templates.lst -dialect netezza -scale 1 -qualify Y` on it.
"""
import re
import shutil
import subprocess
import sys
from pathlib import Path

PARAM = re.compile(r"([A-Za-z_][A-Za-z_0-9]*?)(?:\.(\d+))?\s*=\s*")


def appendix_b(spec_pdf):
    text = subprocess.run(["pdftotext", "-layout", spec_pdf, "-"], capture_output=True, check=True).stdout.decode("utf-8", "replace")
    start = text.index("B.1 query1.tpl")
    sections = {}
    current = None
    for line in text[start:].splitlines():
        m = re.match(r"\s*B\.\d+\s+query(\d+)\.tpl", line)
        if m:
            current = int(m.group(1))
            sections[current] = {}
            continue
        if current is None or "Qualification Substitution Parameters" in line or line.startswith("TPC Benchmark"):
            continue
        # a line holds one or several `NAME.NN = value` entries, a bullet in front
        body = line.lstrip(" \t•�*-")
        found = list(PARAM.finditer(body))
        if not found or found[0].start() != 0:
            continue
        for i, m in enumerate(found):
            end = found[i + 1].start() if i + 1 < len(found) else len(body)
            value = body[m.end():end].strip()
            sections[current].setdefault(m.group(1), {})[int(m.group(2) or 1)] = value
    return sections


def main():
    spec, src, out = sys.argv[1:4]
    params = appendix_b(spec)
    shutil.rmtree(out, ignore_errors=True)
    shutil.copytree(src, out)
    missing = []
    for n in range(1, 100):
        path = Path(out) / f"query{n}.tpl"
        if n not in params or not path.exists():
            missing.append(n)
            continue
        text = path.read_text(encoding="latin-1")
        for name, values in params[n].items():
            def sub(m, values=values):
                k = int(m.group(1)) if m.group(1) else 1
                return values.get(k, m.group(0))
            text = re.sub(r"\[" + re.escape(name) + r"(?:\.(\d+))?\]", sub, text, flags=re.I)
        path.write_text(text, encoding="latin-1", newline="")
    print(f"{len(params)} queries with parameters; no parameters for {missing}", file=sys.stderr)


if __name__ == "__main__":
    main()
