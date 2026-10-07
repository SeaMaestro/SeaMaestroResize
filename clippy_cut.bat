@echo off
rem Same compile-time paths as build_release.bat / test_cut.bat: clippy needs them too, otherwise
rem cut.rs and ort_runtime.rs cannot resolve their include_bytes!/env! arguments.
setlocal
set MODEL=%~dp0models\BEN2_Base.onnx
if not "%~1"=="" set MODEL=%~1
set SEAMAESTRO_CUT_MODEL=%MODEL%
if not defined SEAMAESTRO_ORT_DLL        set SEAMAESTRO_ORT_DLL=%~dp0runtime\onnxruntime.dll
if not defined SEAMAESTRO_DML_DLL        set SEAMAESTRO_DML_DLL=%~dp0runtime\DirectML.dll
if not defined SEAMAESTRO_ORT_SHARED_DLL set SEAMAESTRO_ORT_SHARED_DLL=%~dp0runtime\onnxruntime_providers_shared.dll
echo model: %SEAMAESTRO_CUT_MODEL%
cargo clippy --features bg --all-targets %*
exit /b %errorlevel%
