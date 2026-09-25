@echo off
setlocal
cd /d "%~dp0"

set MODEL=%~dp0models\BEN2_Base.onnx
if not "%~1"=="" set MODEL=%~1
if not exist "%MODEL%" (
  echo model not found: %MODEL%
  exit /b 1
)

set BEFORE=
for %%A in (dist\SeaMaestroCut.exe) do set BEFORE=%%~tA

set SEAMAESTRO_CUT_MODEL=%MODEL%
echo model: %SEAMAESTRO_CUT_MODEL%
echo.

call build_release.bat
set RC=%errorlevel%
if not "%RC%"=="0" (
  echo BUILD FAILED rc=%RC%
  exit /b %RC%
)

set AFTER=
for %%A in (dist\SeaMaestroCut.exe) do set AFTER=%%~tA
if "%BEFORE%"=="%AFTER%" (
  echo WARNING: dist\SeaMaestroCut.exe was not rebuilt, timestamp still %AFTER%
  exit /b 3
)
echo rebuilt: dist\SeaMaestroCut.exe  old %BEFORE%  new %AFTER%
echo next: run_gate.cmd c3_cut_mode_20260922
endlocal
