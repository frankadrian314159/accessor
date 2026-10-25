#!/bin/bash
# Builds the JDBC driver and runs its tests.
#
#   ./build.sh                 compile and test
#   ./build.sh --jar           also write build/accessor-jdbc.jar, with the native library inside it
#
# Needs a JDK of version 22 or later (the driver uses the foreign-function API) and the native
# library, built with `cargo build -p accessor-capi` (set ACCESSOR_LIBRARY to use another file).
set -euo pipefail

# (on Windows, pwd -W gives paths that java.exe understands)
here="$(cd "$(dirname "$0")" && { pwd -W 2>/dev/null || pwd; })"
root="$(cd "$here/.." && { pwd -W 2>/dev/null || pwd; })"

case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) os=windows; lib=accessor_capi.dll; sep=';' ;;
    Darwin) os=macos; lib=libaccessor_capi.dylib; sep=':' ;;
    *) os=linux; lib=libaccessor_capi.so; sep=':' ;;
esac
case "$(uname -m)" in
    arm64|aarch64) arch=aarch64 ;;
    *) arch=x86_64 ;;
esac
library="${ACCESSOR_LIBRARY:-${CARGO_TARGET_DIR:-$root/target}/debug/$lib}"
[ -f "$library" ] || { echo "native library not found: $library (run: cargo build -p accessor-capi)" >&2; exit 1; }

classes="$here/build/classes"
tests="$here/build/test-classes"
rm -rf "$classes" "$tests"
mkdir -p "$classes" "$tests"

# The deprecation notes come from the generated stubs, which must implement the deprecated methods too.
javac --release 22 -Xlint:all,-restricted,-deprecation -Werror -d "$classes" $(find "$here/src/main/java" -name '*.java')
cp -r "$here/src/main/resources/." "$classes/"
javac --release 22 -Xlint:all,-restricted -cp "$classes" -d "$tests" $(find "$here/src/test/java" -name '*.java')

java --enable-native-access=ALL-UNNAMED "-Daccessor.library=$library" -cp "$classes$sep$tests" com.accessor.jdbc.JdbcTest

if [ "${1:-}" = "--jar" ]; then
    mkdir -p "$classes/native/$os-$arch"
    cp "$library" "$classes/native/$os-$arch/$lib"
    jar --create --file "$here/build/accessor-jdbc.jar" -C "$classes" .
    echo "wrote build/accessor-jdbc.jar"
fi
