@echo off
REM ============================================================
REM  Mirr launcher — starts the app; Mirr itself starts and
REM  supervises the AirPlay engine (no consoles, no extra windows).
REM  Prefers the release build, falls back to the debug build.
REM ============================================================
if exist "%~dp0src-tauri\target\release\mirr.exe" (
  start "" "%~dp0src-tauri\target\release\mirr.exe"
) else if exist "%~dp0src-tauri\target\debug\mirr.exe" (
  start "" "%~dp0src-tauri\target\debug\mirr.exe"
) else (
  echo Mirr is not built yet. Run:  cd src-tauri ^&^& cargo build --release
  pause
)
