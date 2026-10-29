#!/usr/bin/env bash
# Builds dsdgen and dsqgen from the TPC-DS kit (the third_party/tpcds-kit submodule) and writes the data and the
# qualification queries for a scale factor, for `accessor-tpcds --kit DIR`. Run it where gcc, make, flex and bison
# exist (on Windows: in WSL), plus python3 and pdftotext (poppler) for the qualification values.
#
#   scripts/tpcds-kit.sh OUT_DIR [SCALE]        SCALE defaults to 1 (the answer sets are for scale 1)
#
# OUT_DIR gets: kit/ (a copy of the kit with LF line endings, built), data/*.dat, queries/query_0.sql,
# tpcds.sql (the schema) and answers/ (the kit's answer sets).
set -euo pipefail
out=${1:?usage: tpcds-kit.sh OUT_DIR [SCALE]}
scale=${2:-1}
here=$(cd "$(dirname "$0")/.." && pwd)
src=$here/third_party/tpcds-kit
[ -f "$src/tools/makefile" ] || { echo "the submodule is missing: git submodule update --init" >&2; exit 1; }
mkdir -p "$out"
out=$(cd "$out" && pwd)

# git on Windows checks the kit out with CRLF line endings, which the kit's parsers cannot read
rm -rf "$out/kit"
cp -r "$src" "$out/kit"
find "$out/kit/tools" "$out/kit/query_templates" "$out/kit/answer_sets" -type f -exec sed -i 's/\r$//' {} +
(cd "$out/kit/tools" && make OS=LINUX CFLAGS="-g -fcommon -D_FILE_OFFSET_BITS=64 -D_LARGEFILE_SOURCE -DLINUX -O2 -w" >"$out/build.log" 2>&1)

rm -rf "$out/data" "$out/queries"
mkdir -p "$out/data" "$out/queries"
(cd "$out/kit/tools" && ./dsdgen -scale "$scale" -dir "$out/data" -force -terminate n >/dev/null)
# dsqgen's -qualify only orders the queries; the qualification values of the specification (Appendix B) go into a copy of
# the templates, so that the answer sets apply. -qualify must be an upper case Y: a lower case y is ignored.
python3 "$here/scripts/tpcds_qualify.py" "$out/kit/specification/TPC-DS_v2.10.0.pdf" "$out/kit/query_templates" "$out/qtemplates"
(cd "$out/kit/tools" && ./dsqgen -directory ../../qtemplates -input ../../qtemplates/templates.lst \
    -dialect netezza -scale "$scale" -qualify Y -output_dir "$out/queries" >/dev/null)
cp "$out/kit/tools/tpcds.sql" "$out/tpcds.sql"
rm -rf "$out/answers"
cp -r "$out/kit/answer_sets" "$out/answers"
echo "scale $scale: $(ls "$out/data" | wc -l) data files, $(grep -c '^-- start query' "$out/queries/query_0.sql") queries in $out"
