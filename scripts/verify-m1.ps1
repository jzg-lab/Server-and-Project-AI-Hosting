[CmdletBinding()]
param(
  [int]$M0Port = 18787,
  [int]$M1Port = 18788
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$repoRoot = Split-Path -Parent $PSScriptRoot
$artifactRoot = Join-Path $repoRoot "artifacts\m1-ssh-fact-chain-20260811"
$runtimeDir = Join-Path $artifactRoot "runtime"
$server = $null
$oldBind = $env:NETWORK_ATLAS_BIND
$oldDatabaseUrl = $env:NETWORK_ATLAS_DATABASE_URL
$oldDataDir = $env:NETWORK_ATLAS_DATA_DIR

function Invoke-NativeStep {
  param(
    [Parameter(Mandatory = $true)][string]$Label,
    [Parameter(Mandatory = $true)][string]$Command,
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$Arguments
  )
  Write-Output "STEP $Label"
  & $Command @Arguments
  if ($LASTEXITCODE -ne 0) {
    throw "$Label failed with exit status $LASTEXITCODE"
  }
  Write-Output "PASS $Label exit=0"
}

function Assert-OpenApiHeader {
  param(
    [Parameter(Mandatory = $true)]$OpenApi,
    [Parameter(Mandatory = $true)][string]$Path
  )
  $operation = $OpenApi.paths.PSObject.Properties[$Path].Value.post
  if (-not $operation) { throw "OpenAPI is missing POST $Path" }
  $header = @($operation.parameters | Where-Object { $_.name -eq "Idempotency-Key" })
  if ($header.Count -ne 1 -or -not $header[0].required) {
    throw "OpenAPI POST $Path must require Idempotency-Key"
  }
}

Push-Location $repoRoot
try {
  New-Item -ItemType Directory -Force -Path $runtimeDir | Out-Null
  New-Item -ItemType Directory -Force -Path (Join-Path $runtimeDir "data") | Out-Null

  Invoke-NativeStep "rust-format" "cargo" "fmt" "--all" "--" "--check"
  Invoke-NativeStep "rust-check" "cargo" "check" "--workspace"
  Invoke-NativeStep "rust-build" "cargo" "build" "-p" "network-atlas"
  Invoke-NativeStep "rust-tests" "cargo" "test" "--workspace"
  Invoke-NativeStep "rust-clippy" "cargo" "clippy" "--workspace" "--all-targets" "--" "-D" "warnings"
  Invoke-NativeStep "sqlite-migration-repeat" "cargo" "test" "--test" "m0_contract" "file_database_uses_wal_and_migrations_are_repeatable" "--" "--exact"
  Invoke-NativeStep "openapi-export" "cargo" "run" "-p" "network-atlas" "--" "--export-openapi" "openapi/openapi.json"
  Invoke-NativeStep "node-install" "npm" "ci"
  Invoke-NativeStep "frontend-types" "npm" "run" "generate:types"
  Invoke-NativeStep "frontend-check" "npm" "run" "check:frontend"
  Invoke-NativeStep "frontend-tests" "npm" "run" "test:frontend"

  $openApi = Get-Content -LiteralPath (Join-Path $repoRoot "openapi\openapi.json") -Raw -Encoding utf8 | ConvertFrom-Json
  foreach ($path in @(
      "/api/v1/secret-refs",
      "/api/v1/hosts",
      "/api/v1/hosts/{host_id}/connection-tests",
      "/api/v1/hosts/{host_id}/host-key-confirmations",
      "/api/v1/hosts/{host_id}/discovery-runs"
    )) {
    Assert-OpenApiHeader $openApi $path
  }
  Write-Output "PASS openapi-idempotency-headers count=5"

  $env:NETWORK_ATLAS_BIND = "127.0.0.1:$M1Port"
  $databasePath = Join-Path $runtimeDir "verify-m1-$PID.db"
  $env:NETWORK_ATLAS_DATABASE_URL = "sqlite://$($databasePath)?mode=rwc"
  $env:NETWORK_ATLAS_DATA_DIR = Join-Path $runtimeDir "data"
  $stdout = Join-Path $runtimeDir "server.stdout.log"
  $stderr = Join-Path $runtimeDir "server.stderr.log"
  $executable = Join-Path $repoRoot "target\debug\network-atlas.exe"
  $server = Start-Process -FilePath $executable -WorkingDirectory $repoRoot `
    -RedirectStandardOutput $stdout -RedirectStandardError $stderr -WindowStyle Hidden -PassThru

  $baseUri = "http://127.0.0.1:$M1Port"
  $deadline = (Get-Date).AddSeconds(20)
  $health = $null
  while ((Get-Date) -lt $deadline) {
    try {
      $health = Invoke-RestMethod -Uri "$baseUri/healthz" -TimeoutSec 2
      break
    } catch {
      Start-Sleep -Milliseconds 250
    }
  }
  if (-not $health -or $health.status -ne "ok") { throw "M1 healthz did not become ready" }
  $hosts = Invoke-RestMethod -Uri "$baseUri/api/v1/hosts" -TimeoutSec 5
  $servedOpenApi = Invoke-RestMethod -Uri "$baseUri/openapi.json" -TimeoutSec 5
  $bootstrap = Invoke-RestMethod -Uri "$baseUri/api/v1/bootstrap" -TimeoutSec 5
  if ($hosts.meta.data_source.kind -ne "real" -or $hosts.data.Count -ne 0) {
    throw "fresh M1 host list did not expose an empty real workspace"
  }
  Assert-OpenApiHeader $servedOpenApi "/api/v1/hosts"
  if ($bootstrap.meta.data_source.kind -ne "fixture") {
    throw "M0 bootstrap fixture marker regressed"
  }
  Write-Output ("PASS m1-api-smoke health={0} hosts={1} source={2}" -f `
      $health.status, $hosts.data.Count, $hosts.meta.data_source.kind)

  $forbidden = @(
    "BEGIN OPENSSH PRIVATE KEY",
    "RAW_DOCUMENT_TOKEN",
    "visible-before-redaction",
    "fixture_document_token_123456789"
  )
  foreach ($file in Get-ChildItem -LiteralPath $runtimeDir -File -ErrorAction SilentlyContinue) {
    $content = Get-Content -LiteralPath $file.FullName -Raw -ErrorAction SilentlyContinue
    foreach ($needle in $forbidden) {
      if ($content -and $content.Contains($needle)) {
        throw "forbidden secret marker found in runtime file $($file.Name)"
      }
    }
  }
  Write-Output "PASS runtime-leak-scan forbidden=0"

  if ($server -and -not $server.HasExited) {
    Stop-Process -Id $server.Id -Force
    $server.WaitForExit()
  }
  $server = $null

  $m0Script = Join-Path $repoRoot "scripts\verify-m0.ps1"
  Write-Output "STEP m0-regression"
  & $m0Script -Port $M0Port
  if ($LASTEXITCODE -ne 0) {
    throw "m0-regression failed with exit status $LASTEXITCODE"
  }
  Write-Output "PASS m0-regression exit=0"
  Write-Output "VERIFY_M1_OK"
} finally {
  if ($server -and -not $server.HasExited) {
    Stop-Process -Id $server.Id -Force
    $server.WaitForExit()
  }
  $env:NETWORK_ATLAS_BIND = $oldBind
  $env:NETWORK_ATLAS_DATABASE_URL = $oldDatabaseUrl
  $env:NETWORK_ATLAS_DATA_DIR = $oldDataDir
  Pop-Location
}
