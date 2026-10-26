# Runs this test (see README.md in the parent folder or the header of the test file).
Set-Location $PSScriptRoot
$jar = if ($env:ACCESSOR_JDBC_JAR) { $env:ACCESSOR_JDBC_JAR } else { "..\jdbc\build\accessor-jdbc.jar" }
if (-not (Test-Path $jar)) { Write-Error "driver jar not found: $jar (run: cd ..\jdbc; bash build.sh --jar)"; exit 1 }
java --enable-native-access=ALL-UNNAMED -cp $jar AccessorJdbcTest.java
exit $LASTEXITCODE
