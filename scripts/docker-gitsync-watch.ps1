$Root = Split-Path -Parent $PSScriptRoot
$Request = Join-Path $Root "DB\gitsync.request"
$env:DOCKER_BUILDKIT = "1"
$env:COMPOSE_DOCKER_CLI_BUILD = "1"
$env:BUILDX_GIT_INFO = "false"

while ($true) {
    if (Test-Path $Request) {
        Remove-Item $Request -Force
        Push-Location $Root
        try {
            docker compose stop pndc
            docker compose build pndc
            docker compose up -d --no-deps --force-recreate pndc
        } finally {
            Pop-Location
        }
    }
    Start-Sleep -Seconds 2
}
