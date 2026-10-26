# Runs this test (see README.md in the parent folder or the header of the test file).
Set-Location $PSScriptRoot
sbcl --noinform --non-interactive --load accessor-cabi-test.lisp
exit $LASTEXITCODE
