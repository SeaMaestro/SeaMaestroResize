@echo off
setlocal
cd /d "%~dp0"

set TAG=%~1
if "%TAG%"=="" set TAG=c3_cut_mode

set GATE=C:\Users\vlady\Desktop\VarTest\_g0_harness\sm_gate_v5.ps1
set REFSRC=C:\Users\vlady\Desktop\VarTest\_g0_manifest\seals_20260912\manifests\sm_manifest_ref_fix4_fragment_20260919.csv

if not exist "%GATE%" (
  echo gate script not found: %GATE%
  exit /b 1
)
if not exist "%REFSRC%" (
  echo reference manifest not found: %REFSRC%
  exit /b 1
)

set EXTRA=-AllowDirty
if /i "%~2"=="clean" set EXTRA=

echo gate tag=%TAG% %EXTRA%
echo reference: %REFSRC%

powershell -NoProfile -ExecutionPolicy Bypass -File "%GATE%" -Tag %TAG% -RefManifest "%REFSRC%" %EXTRA%
if errorlevel 1 (
  echo.
  echo GATE FAILED - see %TEMP%\sm_gate_%TAG%_log.txt
  echo ---- log tail ----
  powershell -NoProfile -ExecutionPolicy Bypass -Command "Get-Content \"$env:TEMP\sm_gate_%TAG%_log.txt\" -Tail 40"
  exit /b 1
)
echo.
echo ---- DONE marker ----
type "%TEMP%\sm_gate_%TAG%_DONE.txt" 2>nul
echo.
echo ---- log tail ----
powershell -NoProfile -ExecutionPolicy Bypass -Command "Get-Content \"$env:TEMP\sm_gate_%TAG%_log.txt\" -Tail 40"
endlocal
