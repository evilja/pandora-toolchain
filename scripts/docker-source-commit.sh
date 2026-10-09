#!/bin/sh
# Resolve a Docker build's revision inside the build image, never through host tooling.
set -eu

source_root=${1:-.}
source_commit=${PANDORA_SOURCE_COMMIT:-}
checkout_commit=$(git -C "$source_root" rev-parse --verify HEAD 2>/dev/null || true)

if [ -z "$source_commit" ]; then
    source_commit=$checkout_commit
fi
case "$source_commit" in
    ''|*[!0-9a-f]*)
        echo 'Cannot resolve the Docker source commit; include the checkout .git directory or set PANDORA_SOURCE_COMMIT to its full SHA' >&2
        exit 1 ;;
esac
if [ "${#source_commit}" -ne 40 ]; then
    echo 'PANDORA_SOURCE_COMMIT must contain exactly 40 lowercase hexadecimal characters' >&2
    exit 1
fi
if [ -n "$checkout_commit" ] && [ "$source_commit" != "$checkout_commit" ]; then
    echo 'PANDORA_SOURCE_COMMIT does not match the checkout HEAD; refusing to label binaries with another revision' >&2
    exit 1
fi
printf '%s\n' "$source_commit"
