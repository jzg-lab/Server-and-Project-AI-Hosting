[CmdletBinding()]
param(
  [int]$Port = 18787
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$repoRoot = Split-Path -Parent $PSScriptRoot
$runtimeDir = Join-Path $repoRoot "artifacts\m0-visual-contract-20260811\runtime"
$server = $null
$oldBind = $env:NETWORK_ATLAS_BIND
$oldDatabaseUrl = $env:NETWORK_ATLAS_DATABASE_URL

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

Push-Location $repoRoot
try {
  New-Item -ItemType Directory -Force -Path $runtimeDir | Out-Null
  New-Item -ItemType Directory -Force -Path (Join-Path $repoRoot "data") | Out-Null

  Invoke-NativeStep "rust-format" "cargo" "fmt" "--all" "--" "--check"
  Invoke-NativeStep "rust-check" "cargo" "check" "--workspace"
  Invoke-NativeStep "rust-tests" "cargo" "test" "--workspace"
  Invoke-NativeStep "rust-clippy" "cargo" "clippy" "--workspace" "--all-targets" "--" "-D" "warnings"
  Invoke-NativeStep "openapi-export" "cargo" "run" "-p" "network-atlas" "--" "--export-openapi" "openapi/openapi.json"
  Invoke-NativeStep "node-install" "npm" "ci"
  Invoke-NativeStep "frontend-types" "npm" "run" "generate:types"
  Invoke-NativeStep "frontend-check" "npm" "run" "check:frontend"
  Invoke-NativeStep "frontend-tests" "npm" "run" "test:frontend"

  $openApi = Get-Content -LiteralPath (Join-Path $repoRoot "openapi\openapi.json") -Raw -Encoding utf8 | ConvertFrom-Json
  if (-not $openApi.components.schemas.GraphHealth) { throw "OpenAPI is missing GraphHealth" }
  Write-Output "PASS openapi-schema GraphHealth=present"

  $env:NETWORK_ATLAS_BIND = "127.0.0.1:$Port"
  $env:NETWORK_ATLAS_DATABASE_URL = "sqlite://data/verify-m0-$PID.db?mode=rwc"
  $stdout = Join-Path $runtimeDir "verify-server.stdout.log"
  $stderr = Join-Path $runtimeDir "verify-server.stderr.log"
  $executable = Join-Path $repoRoot "target\debug\network-atlas.exe"
  $server = Start-Process -FilePath $executable -WorkingDirectory $repoRoot -RedirectStandardOutput $stdout -RedirectStandardError $stderr -WindowStyle Hidden -PassThru

  $baseUri = "http://127.0.0.1:$Port"
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
  if (-not $health -or $health.status -ne "ok") { throw "healthz did not become ready" }

  $bootstrap = Invoke-RestMethod -Uri "$baseUri/api/v1/bootstrap" -TimeoutSec 5
  $world = Invoke-RestMethod -Uri "$baseUri/api/v1/views/global/world" -TimeoutSec 5
  $project = Invoke-RestMethod -Uri "$baseUri/api/v1/projects/hermes/views/resources" -TimeoutSec 5
  $servedOpenApi = Invoke-RestMethod -Uri "$baseUri/openapi.json" -TimeoutSec 5
  $index = Invoke-WebRequest -UseBasicParsing -Uri "$baseUri/" -TimeoutSec 5

  if ($bootstrap.meta.data_source.kind -ne "fixture" -or $bootstrap.meta.freshness -ne "fresh") { throw "bootstrap source metadata mismatch" }
  if ($world.data.nodes.Count -ne 5 -or $world.data.edges.Count -ne 4) { throw "global graph fixture mismatch" }
  if ($project.data.nodes.Count -lt 2 -or $project.data.edges.Count -lt 1) { throw "project graph fixture mismatch" }
  if (-not $servedOpenApi.components.schemas.GraphHealth) { throw "served OpenAPI mismatch" }
  if ($index.StatusCode -ne 200 -or $index.Content -notmatch "data-source.js") { throw "same-origin frontend mismatch" }

  Write-Output ("PASS api-smoke health={0} projects={1} world={2}/{3} project={4}/{5} source={6}/{7} html={8}" -f `
      $health.status,
      $bootstrap.data.projects.Count,
      $world.data.nodes.Count,
      $world.data.edges.Count,
      $project.data.nodes.Count,
      $project.data.edges.Count,
      $bootstrap.meta.data_source.kind,
      $bootstrap.meta.freshness,
      $index.StatusCode)
  Write-Output "VERIFY_M0_OK"
} finally {
  if ($server -and -not $server.HasExited) {
    Stop-Process -Id $server.Id -Force
    $server.WaitForExit()
  }
  $env:NETWORK_ATLAS_BIND = $oldBind
  $env:NETWORK_ATLAS_DATABASE_URL = $oldDatabaseUrl
  Pop-Location
}
