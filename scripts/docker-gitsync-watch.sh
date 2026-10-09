#!/usr/bin/env bash
# Run on the Docker host, not in the container. Keep this process supervised.
set -u

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
request="$root/DB/gitsync.request"
export DOCKER_BUILDKIT=1 COMPOSE_DOCKER_CLI_BUILD=1 BUILDX_GIT_INFO=false

while true; do
    if [[ -f "$request" ]]; then
        if ! IFS= read -r commit < "$request" || [[ ! "$commit" =~ ^[0-9a-f]{40}$ ]]; then
            echo '[gitsync] rebuild request must contain the checkout SHA; request retained (update Pandora and request gitsync again)' >&2
        elif (cd "$root" && PANDORA_SOURCE_COMMIT="$commit" docker compose build pndc &&
              PANDORA_SOURCE_COMMIT="$commit" docker compose up -d --no-deps --force-recreate pndc); then
            if rm -- "$request"; then
                echo "[gitsync] coordinator binaries published for $commit"
            else
                echo '[gitsync] coordinator restarted but could not clear the request' >&2
            fi
        else
            echo '[gitsync] build or restart failed; request retained for retry' >&2
        fi
    fi
    sleep 5
done
