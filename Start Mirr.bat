@echo off
setlocal
cd /d "%~dp0"

REM Mirr Launcher — AirPlay Receiver & LocalSend Ecosystem
REM Automatically launches the newest compiled binary (Release or Debug)

set "REL=%~dp0src-tauri\target\release\mirr.exe"
set "DBG=%~dp0src-tauri\target\debug\mirr.exe"
set "TARGET="

if exist "%REL%" set "TARGET=%REL%"
if exist "%DBG%" if not defined TARGET set "TARGET=%DBG%"

if exist "%REL%" if exist "%DBG%" call :pick_newest

if not defined TARGET (
    echo [Mirr] No compiled executable found.
    echo Building Mirr in release mode...
    cd "%~dp0src-tauri" && cargo build --release
    cd /d "%~dp0"
    if exist "%REL%" set "TARGET=%REL%"
    if exist "%DBG%" set "TARGET=%DBG%"
)

if not defined TARGET (
    echo.
    echo [Mirr] Error: Build failed or executable could not be found.
    pause
    exit /b 1
)

start "" "%TARGET%"
exit /b 0

:pick_newest
for /f "delims=" %%I in ('powershell -NoProfile -ExecutionPolicy Bypass -Command "(Get-Item -LiteralPath '%REL%', '%DBG%' | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName"') do set "TARGET=%%I"
exit /b 0
