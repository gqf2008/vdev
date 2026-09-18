@echo off
rem Build the vdev APO probe DLL (minimal SFX/MFX APO).
rem Requires: Visual Studio 2022 (MSVC x64) + Windows SDK audioenginebaseapo.h.
rem Usage: build.bat [outdir]   default: <repo>\apo-build
setlocal
set OUTDIR=%~1
if "%OUTDIR%"=="" set OUTDIR=%~dp0..\..\..\..\apo-build
if not exist "%OUTDIR%" mkdir "%OUTDIR%"
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul 2>&1
if errorlevel 1 ( echo VCVARS_FAIL & exit /b 1 )
pushd "%~dp0"
cl /nologo /LD /EHsc /O2 /W3 /D_CRT_SECURE_NO_WARNINGS /Fo:"%OUTDIR%\\" /Fe:"%OUTDIR%\vdevapo.dll" vdevapo.cpp ^
   /link /DEF:vdevapo.def ole32.lib uuid.lib user32.lib advapi32.lib
set RC=%ERRORLEVEL%
popd
echo BUILD_EXIT=%RC%
exit /b %RC%
