[CmdletBinding()]
param(
  [int]$AppPort = 18798,
  [int]$HttpsPort = 18443,
  [string]$CaddyPath = "target/m4-tools/release/caddy.exe",
  [string]$Password = "m4-browser-pass"
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
Set-StrictMode -Version Latest
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$caddy = (Resolve-Path (Join-Path $repoRoot $CaddyPath)).Path
$origin = "https://localhost:$HttpsPort"
$runtime = $null
$managedEnvironment = @(
  "NETWORK_ATLAS_BIND",
  "NETWORK_ATLAS_DATA_DIR",
  "NETWORK_ATLAS_DATABASE_URL",
  "NETWORK_ATLAS_FRONTEND_DIR",
  "NETWORK_ATLAS_AUTH_MODE",
  "NETWORK_ATLAS_OWNER_USERNAME",
  "NETWORK_ATLAS_OWNER_PASSWORD_HASH",
  "NETWORK_ATLAS_ALLOWED_ORIGIN",
  "NETWORK_ATLAS_COOKIE_SECURE",
  "XDG_DATA_HOME",
  "XDG_CONFIG_HOME"
)
$previousEnvironment = @{}
foreach ($name in $managedEnvironment) {
  $previousEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
}

foreach ($port in @($AppPort, $HttpsPort)) {
  if (Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue) {
    throw "Port $port is already in use"
  }
}

Push-Location $repoRoot
$appProcess = $null
$caddyProcess = $null
try {
  $caddyVersionOutput = & $caddy version
  if ($LASTEXITCODE -ne 0) { throw "Caddy executable failed" }
  $caddyVersion = ([string]@($caddyVersionOutput)[0]).Split(' ')[0]
  cargo build --quiet -p network-atlas
  if ($LASTEXITCODE -ne 0) { throw "Rust build failed" }

  $binary = (Resolve-Path "target/debug/network-atlas.exe").Path
  $runtime = Join-Path $repoRoot ("target/m4-https-" + [Guid]::NewGuid().ToString("N"))
  New-Item -ItemType Directory -Force -Path $runtime | Out-Null
  $hash = $Password | & $binary --hash-password
  if ($LASTEXITCODE -ne 0 -or $hash -notmatch '^\$argon2id\$') {
    throw "Argon2id hash generation failed"
  }

  $caddyConfig = Join-Path $runtime "Caddyfile"
  $config = @"
{
    skip_install_trust
    admin off
}

https://localhost:$HttpsPort {
    tls internal
    encode gzip
    header {
        Strict-Transport-Security "max-age=300"
        X-Content-Type-Options "nosniff"
        Referrer-Policy "no-referrer"
        -Server
    }
    reverse_proxy 127.0.0.1:$AppPort {
        flush_interval -1
    }
}
"@
  [IO.File]::WriteAllText($caddyConfig, $config, [Text.UTF8Encoding]::new($false))
  & $caddy validate --config $caddyConfig --adapter caddyfile
  if ($LASTEXITCODE -ne 0) { throw "Local Caddy configuration is invalid" }

  $env:NETWORK_ATLAS_BIND = "127.0.0.1:$AppPort"
  $env:NETWORK_ATLAS_DATA_DIR = Join-Path $runtime "data"
  $databasePath = (Join-Path $runtime "data/network-atlas.db").Replace('\', '/')
  $env:NETWORK_ATLAS_DATABASE_URL = "sqlite://$databasePath`?mode=rwc"
  $env:NETWORK_ATLAS_FRONTEND_DIR = (Resolve-Path "frontend").Path
  $env:NETWORK_ATLAS_AUTH_MODE = "required"
  $env:NETWORK_ATLAS_OWNER_USERNAME = "owner"
  $env:NETWORK_ATLAS_OWNER_PASSWORD_HASH = $hash
  $env:NETWORK_ATLAS_ALLOWED_ORIGIN = $origin
  $env:NETWORK_ATLAS_COOKIE_SECURE = "true"
  $env:XDG_DATA_HOME = Join-Path $runtime "caddy-data"
  $env:XDG_CONFIG_HOME = Join-Path $runtime "caddy-config"

  $appProcess = Start-Process -FilePath $binary -WindowStyle Hidden -PassThru `
    -RedirectStandardOutput (Join-Path $runtime "app.stdout.log") `
    -RedirectStandardError (Join-Path $runtime "app.stderr.log")
  $caddyProcess = Start-Process -FilePath $caddy `
    -ArgumentList @("run", "--config", $caddyConfig, "--adapter", "caddyfile") `
    -WindowStyle Hidden -PassThru `
    -RedirectStandardOutput (Join-Path $runtime "caddy.stdout.log") `
    -RedirectStandardError (Join-Path $runtime "caddy.stderr.log")

  $ready = $false
  for ($attempt = 0; $attempt -lt 60; $attempt += 1) {
    try {
      $code = & curl.exe -k -sS --max-time 2 -o NUL -w '%{http_code}' "$origin/healthz" 2>$null
    } catch {
      $code = ""
    }
    if ($code -eq "200") { $ready = $true; break }
    Start-Sleep -Milliseconds 250
  }
  if (-not $ready) {
    $appLog = Get-Content -Raw -ErrorAction SilentlyContinue (Join-Path $runtime "app.stderr.log")
    $caddyLog = Get-Content -Raw -ErrorAction SilentlyContinue (Join-Path $runtime "caddy.stderr.log")
    throw "HTTPS proxy did not become ready`nAPP=$appLog`nCADDY=$caddyLog"
  }

  $health = & curl.exe -k -sS "$origin/healthz"
  $anonymous = & curl.exe -k -sS -o (Join-Path $runtime "anonymous.json") -w '%{http_code}' "$origin/api/v1/bootstrap"
  $pageHeaders = Join-Path $runtime "page.headers"
  $page = & curl.exe -k -sS -D $pageHeaders "$origin/"
  $loginHeaders = Join-Path $runtime "login.headers"
  $cookieJar = Join-Path $runtime "cookies.txt"
  $loginRequest = Join-Path $runtime "login-request.json"
  $loginBody = @{ username = "owner"; password = $Password } | ConvertTo-Json -Compress
  [IO.File]::WriteAllText($loginRequest, $loginBody, [Text.UTF8Encoding]::new($false))
  $login = & curl.exe -k -sS -D $loginHeaders -c $cookieJar `
    -H "Origin: $origin" -H "Content-Type: application/json" `
    --data-binary "@$loginRequest" "$origin/api/v1/auth/login"
  if ($LASTEXITCODE -ne 0) { throw "HTTPS login request failed" }
  $loginJson = $login | ConvertFrom-Json
  $csrf = $loginJson.data.csrf_token
  if (-not $csrf) { throw "Login response omitted CSRF token" }

  $bootstrap = & curl.exe -k -sS -b $cookieJar -o (Join-Path $runtime "bootstrap.json") -w '%{http_code}' "$origin/api/v1/bootstrap"
  $write = & curl.exe -k -sS -b $cookieJar -o (Join-Path $runtime "model-test.json") -w '%{http_code}' `
    -X POST -H "Origin: $origin" -H "X-CSRF-Token: $csrf" `
    -H "Idempotency-Key: https-smoke-model" "$origin/api/v1/model-provider/test"
  $sseHeaders = Join-Path $runtime "sse.headers"
  $sse = & curl.exe -k -s -N --max-time 3 -D $sseHeaders -b $cookieJar "$origin/api/v1/events/stream" 2>$null
  $sseExit = $LASTEXITCODE
  if ($sseExit -notin @(0, 28)) { throw "SSE curl failed with $sseExit" }
  $csrfRejected = & curl.exe -k -sS -b $cookieJar -o (Join-Path $runtime "csrf-rejected.json") -w '%{http_code}' `
    -X POST -H "Origin: $origin" "$origin/api/v1/auth/logout"
  $logout = & curl.exe -k -sS -b $cookieJar -o (Join-Path $runtime "logout.json") -w '%{http_code}' `
    -X POST -H "Origin: $origin" -H "X-CSRF-Token: $csrf" "$origin/api/v1/auth/logout"
  $revoked = & curl.exe -k -sS -b $cookieJar -o (Join-Path $runtime "revoked.json") -w '%{http_code}' "$origin/api/v1/bootstrap"

  $loginHeaderText = Get-Content -Raw $loginHeaders
  $pageHeaderText = Get-Content -Raw $pageHeaders
  $sseHeaderText = Get-Content -Raw $sseHeaders
  $pageText = $page -join "`n"
  $sseText = $sse -join "`n"
  if ($health -notmatch '"status":"ok"') { throw "Health payload mismatch" }
  if ($anonymous -ne "401" -or $bootstrap -ne "200" -or $write -ne "200" -or
      $csrfRejected -ne "403" -or $logout -ne "200" -or $revoked -ne "401") {
    throw "Unexpected statuses anonymous=$anonymous bootstrap=$bootstrap write=$write csrf=$csrfRejected logout=$logout revoked=$revoked"
  }
  if ($pageText -notmatch "Network Atlas" -or $pageHeaderText -notmatch "Strict-Transport-Security") {
    throw "HTTPS page or proxy security header is missing"
  }
  if ($loginHeaderText -notmatch "Secure" -or $loginHeaderText -notmatch "HttpOnly" -or
      $loginHeaderText -notmatch "SameSite=Strict") {
    throw "Session cookie attributes are missing"
  }
  if ($sseHeaderText -notmatch "text/event-stream" -or $sseHeaderText -notmatch "X-Accel-Buffering: no" -or
      $sseText -notmatch "event: onboarding.changed") {
    throw "SSE proxy stream contract is missing"
  }

  Write-Output "HTTPS_SMOKE_OK caddy=$caddyVersion health=200 anonymous=401 login=200 bootstrap=200 csrf=403 sse=onboarding.changed logout=200 revoked=401 cookie=Secure,HttpOnly,SameSite-Strict"
} finally {
  if ($caddyProcess -and -not $caddyProcess.HasExited) {
    Stop-Process -Id $caddyProcess.Id -Force
    Wait-Process -Id $caddyProcess.Id -ErrorAction SilentlyContinue
  }
  if ($appProcess -and -not $appProcess.HasExited) {
    Stop-Process -Id $appProcess.Id -Force
    Wait-Process -Id $appProcess.Id -ErrorAction SilentlyContinue
  }
  foreach ($name in $managedEnvironment) {
    [Environment]::SetEnvironmentVariable($name, $previousEnvironment[$name], "Process")
  }
  Pop-Location
  if ($runtime) {
    $runtimeFull = [IO.Path]::GetFullPath($runtime)
    $targetFull = [IO.Path]::GetFullPath((Join-Path $repoRoot "target"))
    $targetPrefix = $targetFull.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if ($runtimeFull.StartsWith($targetPrefix, [StringComparison]::OrdinalIgnoreCase) -and
        (Split-Path -Leaf $runtimeFull).StartsWith("m4-https-", [StringComparison]::Ordinal)) {
      Remove-Item -LiteralPath $runtimeFull -Recurse -Force -ErrorAction SilentlyContinue
    }
  }
}
