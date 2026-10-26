# Runs this test (see README.md in the parent folder or the header of the test file).
Set-Location $PSScriptRoot
if (-not (Test-Path ..\..\jdbc\build\accessor-jdbc.jar)) { Write-Error "driver jar not found: run (cd ..\..\jdbc; bash build.sh --jar)"; exit 1 }
clojure -M:test
exit $LASTEXITCODE
