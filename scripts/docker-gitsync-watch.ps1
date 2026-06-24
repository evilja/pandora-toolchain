$Root = Split-Path -Parent $PSScriptRoot
$Request = Join-Path $Root "DB\gitsync.request"

while ($true) {
    if (Test-Path $Request) {
        Remove-Item $Request -Force
        Push-Location $Root
        try {
            docker compose up -d --build --force-recreate pndc
        } finally {
            Pop-Location
        }
    }
    Start-Sleep -Seconds 2
}
