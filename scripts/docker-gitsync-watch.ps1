$Root = Split-Path -Parent $PSScriptRoot
$Request = Join-Path $Root "DB\gitsync.request"
$env:DOCKER_BUILDKIT = "1"
$env:COMPOSE_DOCKER_CLI_BUILD = "1"
$env:BUILDX_GIT_INFO = "false"

while ($true) {
    if (Test-Path $Request) {
        Push-Location $Root
        try {
            $commit = (Get-Content -LiteralPath $Request -Raw -ErrorAction Stop).Trim()
            if ($commit -cnotmatch '^[0-9a-f]{40}$') {
                throw "Rebuild request must contain the checkout SHA; update Pandora and request gitsync again"
            }
            $env:PANDORA_SOURCE_COMMIT = $commit
            # Build all five binaries before touching the running coordinator. The image only
            # becomes the served package after Compose successfully recreates the container.
            docker compose build pndc
            if ($LASTEXITCODE -ne 0) { throw "Coordinator image build failed" }
            docker compose up -d --no-deps --force-recreate pndc
            if ($LASTEXITCODE -ne 0) { throw "Coordinator restart failed" }
            Remove-Item $Request -Force -ErrorAction Stop
            Write-Host "[gitsync] coordinator binaries published for $commit"
        } catch {
            Write-Error "[gitsync] $_; keeping $Request for retry" -ErrorAction Continue
        } finally {
            Pop-Location
        }
    }
    Start-Sleep -Seconds 5
}
