pushd %~dp0

:loop
cargo clean -p pandora-toolchain
cargo build --timings
target\debug\pndc.exe
goto loop
