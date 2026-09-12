@echo off
rem Run cargo with MSVC vcvars64 environment (for shells without MSVC env).
rem server-rs is located relative to this script's tools\ dir (repo is relocatable).
rem vcvars64: prefer the default VS2022 BuildTools path, fall back to vswhere probe.
setlocal
set "VCVARS=%ProgramFiles(x86)%\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if exist "%VCVARS%" goto run
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%VSWHERE%" goto missing
for /f "usebackq delims=" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do if exist "%%i\VC\Auxiliary\Build\vcvars64.bat" set "VCVARS=%%i\VC\Auxiliary\Build\vcvars64.bat"
if exist "%VCVARS%" goto run
:missing
echo [cargo-vcvars] vcvars64.bat not found; install VS BuildTools with MSVC toolset 1>&2
exit /b 1
:run
call "%VCVARS%" >nul
cd /d "%~dp0..\server-rs"
%*
