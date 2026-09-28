#!/usr/bin/env bash
set -u
cd "$(dirname "$0")" || exit 1
export PANDORA_BINARY_LAUNCHER=1

root=DB/bin/pandora
while true; do
    if [ -f "$root/source-build.request" ]; then
        if ! cargo build --release --bins; then
            echo "start-node.sh: source fallback build failed; fix the build environment" >&2
            exit 1
        fi
        mv "$root/source-build.request" "$root/source-active" || exit 1
    fi
    if [ -f "$root/source-active" ] && [ -x target/release/pndc ]; then
        binary=target/release/pndc
    elif [ -x "$root/current/pndc" ]; then
        binary="$root/current/pndc"
    elif [ -x target/release/pndc ]; then
        binary=target/release/pndc
    elif [ -x target/debug/pndc ]; then
        binary=target/debug/pndc
    else
        echo "start-node.sh: no pndc binary found; install a bootstrap binary first" >&2
        exit 1
    fi
    launched=$(realpath "$binary") || exit 1
    "./$binary" --mini
    status=$?
    if [ "$status" -eq 78 ]; then
        echo "start-node.sh: Pandora needs configuring; run '$binary --setup'" >&2
        exit 78
    fi
    if [ -f "$root/pending.json" ] && [ -e "$root/current/pndc" ] && [ "$launched" = "$(realpath "$root/current/pndc")" ]; then
        if [ -L "$root/previous" ]; then
            ln -s "$(readlink "$root/previous")" "$root/current.rollback" || exit 1
            mv -Tf "$root/current.rollback" "$root/current" || exit 1
            echo "start-node.sh: new binary failed before registration; restored previous release" >&2
        else
            mv "$root/current" "$root/current.failed" || exit 1
            echo "start-node.sh: new binary failed before registration; restored the bootstrap executable" >&2
        fi
        mv "$root/pending.json" "$root/failed-pending.json" || exit 1
        sleep 10
    fi
done
