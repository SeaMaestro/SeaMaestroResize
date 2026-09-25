@echo off
setlocal
cd /d "%~dp0"

if not defined SEAMAESTRO_CUT_MODEL (
  echo SEAMAESTRO_CUT_MODEL is not set: run build_cut.bat, or point it at models\BEN2_Base.onnx
  exit /b 1
)

if not defined SEAMAESTRO_ORT_DLL        set SEAMAESTRO_ORT_DLL=%~dp0runtime\onnxruntime.dll
if not defined SEAMAESTRO_DML_DLL        set SEAMAESTRO_DML_DLL=%~dp0runtime\DirectML.dll
if not defined SEAMAESTRO_ORT_SHARED_DLL set SEAMAESTRO_ORT_SHARED_DLL=%~dp0runtime\onnxruntime_providers_shared.dll
if not exist "%SEAMAESTRO_ORT_DLL%" (
  echo runtime not found: %SEAMAESTRO_ORT_DLL%
  echo put onnxruntime.dll, DirectML.dll and onnxruntime_providers_shared.dll into runtime\
  exit /b 1
)
if not exist "%SEAMAESTRO_DML_DLL%" (
  echo runtime not found: %SEAMAESTRO_DML_DLL%
  exit /b 1
)
if not exist "%SEAMAESTRO_ORT_SHARED_DLL%" (
  echo runtime not found: %SEAMAESTRO_ORT_SHARED_DLL%
  exit /b 1
)

if not exist dist mkdir dist

echo [1/2] light build (resizer only)
cargo build --release --bin SeaMaestro
if errorlevel 1 exit /b 1
copy /y target\release\SeaMaestro.exe dist\SeaMaestro.exe >nul
if errorlevel 1 exit /b 1

echo [2/2] full build (resizer + background cut)
cargo build --release --features bg
if errorlevel 1 exit /b 1
copy /y target\release\SeaMaestro.exe dist\SeaMaestroCut.exe >nul
if errorlevel 1 exit /b 1

echo.
echo SHA256:
certutil -hashfile dist\SeaMaestro.exe SHA256 | findstr /v ":"
certutil -hashfile dist\SeaMaestroCut.exe SHA256 | findstr /v ":"
echo.
echo dist\SeaMaestro.exe (light) + dist\SeaMaestroCut.exe (full) - the runtime is inside the exe
endlocal
