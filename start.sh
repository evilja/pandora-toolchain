#!/usr/bin/env sh
cd "$(dirname "$0")" || exit 1

while true
do
    cargo clean -p pandora-toolchain
    cargo build --timings
    ./target/debug/pndc
    status=$?
    # EX_CONFIG. Pandora is not configured, and restarting cannot change that — it would only
    # reprint the same instructions until somebody notices.
    if [ "$status" -eq 78 ]; then
        echo "start.sh: Pandora needs configuring; run './target/debug/pndc --setup' and start again."
        exit 78
    fi
done
