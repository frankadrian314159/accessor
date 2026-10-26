#!/bin/bash
# Runs the honey test against Accessor's JDBC driver. Needs the Clojure CLI and ../../jdbc/build/accessor-jdbc.jar
# (build it with: cd ../../jdbc && ./build.sh --jar).
set -euo pipefail
cd "$(dirname "$0")"
[ -f ../../jdbc/build/accessor-jdbc.jar ] || { echo "driver jar not found: run (cd ../../jdbc && ./build.sh --jar)" >&2; exit 1; }
clojure -M:test
