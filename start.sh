#!/usr/bin/env sh
cd "$(dirname "$0")" || exit 1

while true
do
    cargo clean -p pandora-toolchain
    cargo build --timings
    ./target/debug/pndc
done
