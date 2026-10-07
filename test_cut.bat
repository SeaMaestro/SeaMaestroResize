@echo off
rem Unit tests need the same compile-time paths as the release build: build_release.bat sets them
rem inside its own cmd process, so `cargo test --features bg` from a plain shell cannot see them.
setlocal
set MODEL=%~dp0models\BEN2_Base.onnx
if not "%~1"=="" set MODEL=%~1
set SEAMAESTRO_CUT_MODEL=%MODEL%
if not defined SEAMAESTRO_ORT_DLL        set SEAMAESTRO_ORT_DLL=%~dp0runtime\onnxruntime.dll
if not defined SEAMAESTRO_DML_DLL        set SEAMAESTRO_DML_DLL=%~dp0runtime\DirectML.dll
if not defined SEAMAESTRO_ORT_SHARED_DLL set SEAMAESTRO_ORT_SHARED_DLL=%~dp0runtime\onnxruntime_providers_shared.dll
echo model: %SEAMAESTRO_CUT_MODEL%
cargo test --features bg %*
exit /b %errorlevel%
