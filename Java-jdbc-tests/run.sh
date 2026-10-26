#!/bin/bash
# Runs the plain-JDBC test against Accessor's driver (Java 22 or later).
#
# Needs accessor-jdbc.jar: build it with   (cd ../jdbc && ./build.sh --jar)
# (the jar holds the driver and the native library; set ACCESSOR_JDBC_JAR to use another jar).
set -euo pipefail
here="$(cd "$(dirname "$0")" && { pwd -W 2>/dev/null || pwd; })"
jar="${ACCESSOR_JDBC_JAR:-$here/../jdbc/build/accessor-jdbc.jar}"
[ -f "$jar" ] || { echo "driver jar not found: $jar (run: cd ../jdbc && ./build.sh --jar)" >&2; exit 1; }
java --enable-native-access=ALL-UNNAMED -cp "$jar" "$here/AccessorJdbcTest.java"
