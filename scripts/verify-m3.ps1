[CmdletBinding()]
param(
  [int]$Port = 18796
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$repoRoot = Split-Path -Parent $PSScriptRoot
$runtimeDir = Join-Path ([System.IO.Path]::GetTempPath()) ("network-atlas-m3-verify-" + [guid]::NewGuid().ToString("N"))
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

function Invoke-Json {
  param(
    [Parameter(Mandatory = $true)][ValidateSet("GET", "POST", "PUT")][string]$Method,
    [Parameter(Mandatory = $true)][string]$Uri,
    [hashtable]$Body,
    [hashtable]$Headers = @{}
  )
  $params = @{ Method = $Method; Uri = $Uri; TimeoutSec = 20 }
  if ($Body) {
    $params.ContentType = "application/json; charset=utf-8"
    $params.Body = ($Body | ConvertTo-Json -Depth 20 -Compress)
  }
  if ($Headers.Count -gt 0) { $params.Headers = $Headers }
  return Invoke-RestMethod @params
}

function Assert-Equal {
  param(
    [Parameter(Mandatory = $true)]$Actual,
    [Parameter(Mandatory = $true)]$Expected,
    [Parameter(Mandatory = $true)][string]$Label
  )
  if ($Actual -ne $Expected) { throw "$Label expected '$Expected' but got '$Actual'" }
}

function Assert-DoesNotContain {
  param(
    [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content,
    [Parameter(Mandatory = $true)][string]$Needle,
    [Parameter(Mandatory = $true)][string]$Label
  )
  if ($Content.Contains($Needle)) { throw "$Label contains a protected model key" }
}

function Wait-Server {
  param([Parameter(Mandatory = $true)][string]$BaseUri)
  $deadline = (Get-Date).AddSeconds(20)
  while ((Get-Date) -lt $deadline) {
    try {
      $health = Invoke-RestMethod -Uri "$BaseUri/healthz" -TimeoutSec 2
      if ($health.status -eq "ok") { return $health }
    } catch {
      Start-Sleep -Milliseconds 250
    }
  }
  throw "M3 server did not become ready"
}

function Stop-Child {
  param([System.Diagnostics.Process]$Process)
  if ($Process -and -not $Process.HasExited) {
    Stop-Process -Id $Process.Id -Force
    $Process.WaitForExit()
  }
}

function Read-FileForLeakScan {
  param([Parameter(Mandatory = $true)][string]$Path)
  return [System.Text.Encoding]::UTF8.GetString([System.IO.File]::ReadAllBytes($Path))
}

Push-Location $repoRoot
try {
  New-Item -ItemType Directory -Force -Path $runtimeDir | Out-Null

  Invoke-NativeStep "rust-format" "cargo" "fmt" "--all" "--" "--check"
  Invoke-NativeStep "rust-check" "cargo" "check" "--workspace"
  Invoke-NativeStep "rust-build" "cargo" "build" "--workspace" "--examples"
  Invoke-NativeStep "rust-tests" "cargo" "test" "--workspace"
  Invoke-NativeStep "rust-clippy" "cargo" "clippy" "--workspace" "--all-targets" "--" "-D" "warnings"
  Invoke-NativeStep "openapi-export" "cargo" "run" "-p" "network-atlas" "--" "--export-openapi" "openapi/openapi.json"
  Invoke-NativeStep "frontend-types" "npm" "run" "generate:types"
  Invoke-NativeStep "frontend-check" "npm" "run" "check:frontend"
  Invoke-NativeStep "frontend-tests" "npm" "run" "test:frontend"

  $openApiPath = Join-Path $repoRoot "openapi\openapi.json"
  $openApi = Get-Content -LiteralPath $openApiPath -Raw -Encoding utf8 | ConvertFrom-Json
  $requiredOperations = @(
    @{ path = "/api/v1/discovery-runs/{run_id}/diff"; method = "get"; idempotent = $false },
    @{ path = "/api/v1/discovery-runs/{run_id}/proposal"; method = "get"; idempotent = $false },
    @{ path = "/api/v1/model-provider"; method = "get"; idempotent = $false },
    @{ path = "/api/v1/model-provider"; method = "put"; idempotent = $true },
    @{ path = "/api/v1/model-provider/test"; method = "post"; idempotent = $true },
    @{ path = "/api/v1/onboarding-sessions"; method = "post"; idempotent = $true },
    @{ path = "/api/v1/onboarding-sessions/{session_id}"; method = "get"; idempotent = $false },
    @{ path = "/api/v1/onboarding-sessions/{session_id}/messages"; method = "post"; idempotent = $true }
  )
  foreach ($entry in $requiredOperations) {
    $pathProperty = $openApi.paths.PSObject.Properties[$entry.path]
    if (-not $pathProperty) { throw "OpenAPI is missing M3 path $($entry.path)" }
    $operation = $pathProperty.Value.PSObject.Properties[$entry.method].Value
    if (-not $operation) { throw "OpenAPI is missing M3 operation $($entry.method) $($entry.path)" }
    if ($entry.idempotent) {
      $header = @($operation.parameters | Where-Object { $_.name -eq "Idempotency-Key" })
      if ($header.Count -ne 1 -or -not $header[0].required) {
        throw "M3 operation $($entry.method) $($entry.path) must require Idempotency-Key"
      }
    }
  }
  $modelResponseFields = @($openApi.components.schemas.ModelProviderData.properties.PSObject.Properties.Name)
  foreach ($forbiddenField in @("api_key", "credential_ref")) {
    if ($modelResponseFields -contains $forbiddenField) {
      throw "ModelProviderData exposes $forbiddenField"
    }
  }
  foreach ($action in @("adopt", "modify", "reject", "undo", "answer")) {
    if (@($openApi.components.schemas.OnboardingAction.enum) -notcontains $action) {
      throw "OpenAPI is missing onboarding action $action"
    }
  }
  foreach ($kind in @("added", "changed", "missing", "conflict", "unchanged")) {
    if (@($openApi.components.schemas.DiscoveryDiffCounts.properties.PSObject.Properties.Name) -notcontains $kind) {
      throw "OpenAPI is missing discovery diff count $kind"
    }
  }
  Write-Output "PASS openapi-m3-contract routes=8 actions=5 diff-kinds=5 secrets=redacted"

  if (Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue) {
    throw "Port $Port is already in use"
  }
  $databasePath = Join-Path $runtimeDir "verify-m3.db"
  $dataDir = Join-Path $runtimeDir "data"
  $serverOut = Join-Path $runtimeDir "server.stdout.log"
  $serverErr = Join-Path $runtimeDir "server.stderr.log"
  New-Item -ItemType Directory -Force -Path $dataDir | Out-Null
  $env:NETWORK_ATLAS_BIND = "127.0.0.1:$Port"
  $env:NETWORK_ATLAS_DATABASE_URL = "sqlite://$($databasePath)?mode=rwc"
  $env:NETWORK_ATLAS_DATA_DIR = $dataDir
  $serverExe = Join-Path $repoRoot "target\debug\network-atlas.exe"
  $server = Start-Process -FilePath $serverExe -WorkingDirectory $repoRoot `
    -RedirectStandardOutput $serverOut -RedirectStandardError $serverErr `
    -WindowStyle Hidden -PassThru
  $baseUri = "http://127.0.0.1:$Port"
  Wait-Server $baseUri | Out-Null
  Write-Output "PASS m3-healthz"

  $sentinel = "m3-key-$([guid]::NewGuid().ToString('N'))"
  $secret = Invoke-Json "POST" "$baseUri/api/v1/secret-refs" `
    @{ kind = "model_key"; api_key = $sentinel } `
    @{ "Idempotency-Key" = "m3-secret-$PID" }
  $credentialRef = [string]$secret.data.credential_ref
  if (-not $credentialRef.StartsWith("secret://model/")) {
    throw "Model key did not return a model secret reference"
  }
  Assert-DoesNotContain ($secret | ConvertTo-Json -Depth 20 -Compress) $sentinel "secret response"
  $secretId = $credentialRef.Substring("secret://model/".Length)
  $secretPath = Join-Path $dataDir "secrets\$secretId.secret"
  if (-not (Test-Path -LiteralPath $secretPath -PathType Leaf)) {
    throw "Model key was not written to the secret store"
  }
  Assert-Equal ([System.IO.File]::ReadAllText($secretPath)) $sentinel "stored model key"

  $configured = Invoke-Json "PUT" "$baseUri/api/v1/model-provider" `
    @{ base_url = "http://127.0.0.1:1/v1"; model = "verify-model"; credential_ref = $credentialRef } `
    @{ "Idempotency-Key" = "m3-model-$PID" }
  Assert-Equal $configured.data.key_state "stored" "model key state"
  $reopened = Invoke-Json "GET" "$baseUri/api/v1/model-provider"
  Assert-Equal $reopened.data.base_url "http://127.0.0.1:1/v1" "model base URL"
  Assert-Equal $reopened.data.model "verify-model" "model name"
  $modelResponses = ($configured | ConvertTo-Json -Depth 20 -Compress) + ($reopened | ConvertTo-Json -Depth 20 -Compress)
  Assert-DoesNotContain $modelResponses $sentinel "model provider response"
  Assert-DoesNotContain $modelResponses $credentialRef "model provider response"

  $modelTest = Invoke-Json "POST" "$baseUri/api/v1/model-provider/test" $null `
    @{ "Idempotency-Key" = "m3-model-test-$PID" }
  Assert-Equal $modelTest.data.state "failed" "unreachable model state"
  $world = Invoke-Json "GET" "$baseUri/api/v1/views/global/world"
  if (-not $world.data.nodes) { throw "M2 world view stopped working after model failure" }
  Write-Output "PASS m3-model-failure-degrades-without-breaking-m2"

  Stop-Child $server
  $server = $null
  foreach ($path in @(
      $databasePath,
      "$databasePath-shm",
      "$databasePath-wal",
      $serverOut,
      $serverErr,
      $openApiPath,
      (Join-Path $repoRoot "frontend\generated\api.d.ts")
    ) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf }) {
    Assert-DoesNotContain (Read-FileForLeakScan $path) $sentinel (Split-Path -Leaf $path)
  }
  Write-Output "PASS m3-secret-boundary store=file database=redacted logs=redacted responses=redacted contract=redacted"
  Write-Output "VERIFY_M3_OK"
} finally {
  Stop-Child $server
  $env:NETWORK_ATLAS_BIND = $oldBind
  $env:NETWORK_ATLAS_DATABASE_URL = $oldDatabaseUrl
  $env:NETWORK_ATLAS_DATA_DIR = $oldDataDir
  Pop-Location

  $runtimeFull = [System.IO.Path]::GetFullPath($runtimeDir)
  $tempFull = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
  $runtimeLeaf = Split-Path -Leaf $runtimeFull
  if ($runtimeFull.StartsWith($tempFull, [System.StringComparison]::OrdinalIgnoreCase) -and
      $runtimeLeaf.StartsWith("network-atlas-m3-verify-", [System.StringComparison]::Ordinal)) {
    Remove-Item -LiteralPath $runtimeFull -Recurse -Force -ErrorAction SilentlyContinue
  }
}
