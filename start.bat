pushd %~dp0

:loop
cargo clean -p pandora-toolchain
cargo build --timings
target\debug\pndc.exe
rem EX_CONFIG (78): Pandora is not configured, and restarting cannot change that.
rem Stop instead of rebuilding and respinning forever, like start.sh does.
if %ERRORLEVEL%==78 (
  echo start.bat: Pandora needs configuring; run 'target\debug\pndc.exe --setup' and start again.
  exit /b 78
)
goto loop
