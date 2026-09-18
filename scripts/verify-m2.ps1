[CmdletBinding()]
param(
  [int]$Port = 18795,
  [int]$FixturePort = 22225
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$repoRoot = Split-Path -Parent $PSScriptRoot
$artifactRoot = Join-Path $repoRoot "artifacts\m2-visual-management-20260811"
$runtimeDir = Join-Path $artifactRoot "runtime"
$server = $null
$fixture = $null
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
    [Parameter(Mandatory = $true)][ValidateSet("GET", "POST", "PATCH")][string]$Method,
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
  throw "M2 server did not become ready"
}

function Stop-Child {
  param([System.Diagnostics.Process]$Process)
  if ($Process -and -not $Process.HasExited) {
    Stop-Process -Id $Process.Id -Force
    $Process.WaitForExit()
  }
}

Push-Location $repoRoot
try {
  New-Item -ItemType Directory -Force -Path $runtimeDir | Out-Null
  $dataDir = Join-Path $runtimeDir "verify-m2-data-$PID"
  New-Item -ItemType Directory -Force -Path $dataDir | Out-Null
  $clientKeyPath = Join-Path $dataDir "fixture-client-key"

  Invoke-NativeStep "rust-format" "cargo" "fmt" "--all" "--" "--check"
  Invoke-NativeStep "rust-check" "cargo" "check" "--workspace"
  Invoke-NativeStep "rust-build" "cargo" "build" "--workspace" "--examples"
  Invoke-NativeStep "rust-tests" "cargo" "test" "--workspace"
  Invoke-NativeStep "rust-clippy" "cargo" "clippy" "--workspace" "--all-targets" "--" "-D" "warnings"
  Invoke-NativeStep "openapi-export" "cargo" "run" "-p" "network-atlas" "--" "--export-openapi" "openapi/openapi.json"
  Invoke-NativeStep "frontend-types" "npm" "run" "generate:types"
  Invoke-NativeStep "frontend-check" "npm" "run" "check:frontend"
  Invoke-NativeStep "frontend-tests" "npm" "run" "test:frontend"
  # Windows PowerShell 5 drops an empty native argument; the quoted token reaches ssh-keygen as an empty passphrase.
  Invoke-NativeStep "ssh-fixture-client-key" "ssh-keygen" "-q" "-t" "ed25519" "-N" '""' "-f" $clientKeyPath

  $openApi = Get-Content -LiteralPath (Join-Path $repoRoot "openapi\openapi.json") -Raw -Encoding utf8 | ConvertFrom-Json
  foreach ($entry in @(
      @{ path = "/api/v1/projection-drafts/{draft_id}"; method = "patch" },
      @{ path = "/api/v1/projection-drafts/{draft_id}/confirm"; method = "post" },
      @{ path = "/api/v1/layouts/{layout_id}"; method = "patch" },
      @{ path = "/api/v1/ignore-rules"; method = "post" }
    )) {
    $path = $entry.path
    $operation = $openApi.paths.PSObject.Properties[$path].Value.($entry.method)
    if (-not $operation) { throw "OpenAPI is missing M2 operation $path" }
    $header = @($operation.parameters | Where-Object { $_.name -eq "Idempotency-Key" })
    if ($header.Count -ne 1 -or -not $header[0].required) {
      throw "M2 operation $path must require Idempotency-Key"
    }
  }
  Write-Output "PASS openapi-m2-idempotency-headers"

  $fixtureOut = Join-Path $runtimeDir "verify-m2-fixture-$PID.stdout.log"
  $fixtureErr = Join-Path $runtimeDir "verify-m2-fixture-$PID.stderr.log"
  $serverOut = Join-Path $runtimeDir "verify-m2-server-$PID.stdout.log"
  $serverErr = Join-Path $runtimeDir "verify-m2-server-$PID.stderr.log"
  $fixtureExe = Join-Path $repoRoot "target\debug\examples\m2_ssh_fixture.exe"
  $serverExe = Join-Path $repoRoot "target\debug\network-atlas.exe"
  $fixture = Start-Process -FilePath $fixtureExe -WorkingDirectory $repoRoot `
    -ArgumentList "--port", "$FixturePort" -RedirectStandardOutput $fixtureOut `
    -RedirectStandardError $fixtureErr -WindowStyle Hidden -PassThru

  $env:NETWORK_ATLAS_BIND = "127.0.0.1:$Port"
  $databasePath = Join-Path $runtimeDir "verify-m2-$PID.db"
  $env:NETWORK_ATLAS_DATABASE_URL = "sqlite://$($databasePath)?mode=rwc"
  $env:NETWORK_ATLAS_DATA_DIR = $dataDir
  $server = Start-Process -FilePath $serverExe -WorkingDirectory $repoRoot `
    -RedirectStandardOutput $serverOut -RedirectStandardError $serverErr `
    -WindowStyle Hidden -PassThru
  $baseUri = "http://127.0.0.1:$Port"
  Wait-Server $baseUri | Out-Null
  Write-Output "PASS m2-healthz"

  $privateKey = [System.IO.File]::ReadAllText($clientKeyPath)
  $secret = Invoke-Json "POST" "$baseUri/api/v1/secret-refs" `
    @{ kind = "ssh_key"; private_key = $privateKey } `
    @{ "Idempotency-Key" = "m2-secret-$PID" }
  $createdHost = Invoke-Json "POST" "$baseUri/api/v1/hosts" `
    @{ display_name = "M2 API fixture"; address = "127.0.0.1"; port = $FixturePort; ssh_user = "fixture"; credential_ref = $secret.data.credential_ref } `
    @{ "Idempotency-Key" = "m2-host-$PID" }
  $hostId = $createdHost.data.host_id
  $connection = Invoke-Json "POST" "$baseUri/api/v1/hosts/$hostId/connection-tests" $null @{ "Idempotency-Key" = "m2-fingerprint-$PID" }
  Assert-Equal $connection.data.state "host_key_unverified" "initial fingerprint state"
  $confirmedHost = Invoke-Json "POST" "$baseUri/api/v1/hosts/$hostId/host-key-confirmations" `
    @{ fingerprint = $connection.data.candidate_fingerprint } `
    @{ "Idempotency-Key" = "m2-confirm-key-$PID" }
  Assert-Equal $confirmedHost.data.host_key_state "verified" "confirmed host key state"
  $ready = Invoke-Json "POST" "$baseUri/api/v1/hosts/$hostId/connection-tests" $null @{ "Idempotency-Key" = "m2-connect-$PID" }
  Assert-Equal $ready.data.state "connection_ready" "connection state"
  $accepted = Invoke-Json "POST" "$baseUri/api/v1/hosts/$hostId/discovery-runs" @{} @{ "Idempotency-Key" = "m2-discovery-$PID" }

  $run = $null
  $deadline = (Get-Date).AddSeconds(20)
  while ((Get-Date) -lt $deadline) {
    $run = Invoke-RestMethod -Uri "$baseUri/api/v1/discovery-runs/$($accepted.data.run_id)" -TimeoutSec 5
    if ($run.data.state -eq "evidence_ready") { break }
    if ($run.data.state -notin @("accepted", "running")) { throw "M2 discovery failed: $($run.data.failure_code)" }
    Start-Sleep -Milliseconds 250
  }
  Assert-Equal $run.data.state "evidence_ready" "discovery state"
  if (-not $run.data.draft_id) { throw "evidence_ready discovery did not expose draft_id" }
  $draftId = $run.data.draft_id
  $draft = Invoke-RestMethod -Uri "$baseUri/api/v1/projection-drafts/$draftId" -TimeoutSec 5
  $kinds = @($draft.data.nodes | ForEach-Object { $_.kind })
  foreach ($kind in @("host", "project", "compose_project", "service", "container", "image", "network", "volume", "port", "document")) {
    if ($kinds -notcontains $kind) { throw "M2 draft is missing node kind $kind" }
  }
  Write-Output ("PASS m2-real-draft nodes={0} edges={1} source=document" -f $draft.data.nodes.Count, $draft.data.edges.Count)

  $container = @($draft.data.nodes | Where-Object { $_.kind -eq "container" })[0]
  $revision = [int64]$draft.data.revision
  $rename = Invoke-Json "PATCH" "$baseUri/api/v1/projection-drafts/$draftId" `
    @{ base_revision = $revision; operations = @(@{ op = "rename"; node_id = $container.id; label = "M2 API Gateway" }) } `
    @{ "If-Match" = "revision-$revision"; "Idempotency-Key" = "m2-rename-$PID" }
  $revision = [int64]$rename.data.revision
  $project = Invoke-Json "PATCH" "$baseUri/api/v1/projection-drafts/$draftId" `
    @{ base_revision = $revision; operations = @(@{ op = "create_project"; project_id = "m2-local-project"; label = "M2 Local Project"; subtitle = "verification boundary" }) } `
    @{ "If-Match" = "revision-$revision"; "Idempotency-Key" = "m2-project-$PID" }
  $revision = [int64]$project.data.revision
  $assigned = Invoke-Json "PATCH" "$baseUri/api/v1/projection-drafts/$draftId" `
    @{ base_revision = $revision; operations = @(@{ op = "assign_project"; node_id = $container.id; project_id = "m2-local-project" }) } `
    @{ "If-Match" = "revision-$revision"; "Idempotency-Key" = "m2-assign-$PID" }
  $revision = [int64]$assigned.data.revision
  $relation = Invoke-Json "PATCH" "$baseUri/api/v1/projection-drafts/$draftId" `
    @{ base_revision = $revision; operations = @(@{ op = "add_relation"; from = "m2-local-project"; to = $container.id; kind = "contains"; label = "verification relation" }) } `
    @{ "If-Match" = "revision-$revision"; "Idempotency-Key" = "m2-relation-$PID" }
  $revision = [int64]$relation.data.revision
  $layoutId = "layout-$draftId"
  $layout = Invoke-Json "PATCH" "$baseUri/api/v1/layouts/$layoutId" `
    @{ base_revision = $revision; positions = @(@{ node_id = $container.id; position = @{ x = 480.0; y = 260.0 } }) } `
    @{ "If-Match" = "revision-$revision"; "Idempotency-Key" = "m2-layout-$PID" }
  $revision = [int64]$layout.data.revision
  $archived = Invoke-Json "POST" "$baseUri/api/v1/ignore-rules" `
    @{ draft_id = $draftId; node_id = $container.id; action = "archive"; base_revision = $revision } `
    @{ "If-Match" = "revision-$revision"; "Idempotency-Key" = "m2-archive-$PID" }
  $revision = [int64]$archived.data.revision
  $restored = Invoke-Json "POST" "$baseUri/api/v1/ignore-rules" `
    @{ draft_id = $draftId; node_id = $container.id; action = "restore"; base_revision = $revision } `
    @{ "If-Match" = "revision-$revision"; "Idempotency-Key" = "m2-restore-$PID" }
  $revision = [int64]$restored.data.revision
  $version = Invoke-Json "POST" "$baseUri/api/v1/projection-drafts/$draftId/confirm" `
    @{ base_revision = $revision } `
    @{ "If-Match" = "revision-$revision"; "Idempotency-Key" = "m2-confirm-$PID" }
  Assert-Equal $version.data.nodes[0].state "confirmed" "confirmed projection state"
  $world = Invoke-RestMethod -Uri "$baseUri/api/v1/views/global/world" -TimeoutSec 5
  Assert-Equal $world.meta.data_source.kind "real" "world data source"
  $reopened = Invoke-RestMethod -Uri "$baseUri/api/v1/projection-drafts/$draftId" -TimeoutSec 5
  Assert-Equal $reopened.data.state "confirmed" "reopened draft state"
  $persisted = @($reopened.data.nodes | Where-Object { $_.id -eq $container.id })[0]
  Assert-Equal $persisted.label "M2 API Gateway" "persisted rename"
  Assert-Equal ([double]$persisted.position.x) 480.0 "persisted layout x"
  Write-Output ("PASS m2-management revision={0} confirmed={1} layout=480,260" -f $revision, $version.data.version_id)

  $forbidden = @("BEGIN OPENSSH PRIVATE KEY", "fixture-document-secret", "RAW_DOCUMENT_TOKEN")
  $runtimeOutputs = @(
    $fixtureOut,
    $fixtureErr,
    $serverOut,
    $serverErr,
    $databasePath,
    "$databasePath-shm",
    "$databasePath-wal"
  )
  foreach ($path in $runtimeOutputs | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf }) {
    $content = Get-Content -LiteralPath $path -Raw -ErrorAction SilentlyContinue
    foreach ($needle in $forbidden) {
      if ($content -and $content.Contains($needle)) { throw "forbidden marker found in $(Split-Path -Leaf $path)" }
    }
  }
  Write-Output "PASS m2-runtime-leak-scan forbidden=0"
  Write-Output "VERIFY_M2_OK"
} finally {
  Stop-Child $server
  Stop-Child $fixture
  $env:NETWORK_ATLAS_BIND = $oldBind
  $env:NETWORK_ATLAS_DATABASE_URL = $oldDatabaseUrl
  $env:NETWORK_ATLAS_DATA_DIR = $oldDataDir
  Pop-Location
}
