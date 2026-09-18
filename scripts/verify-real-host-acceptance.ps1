[CmdletBinding()]
param(
  [string]$BaseUrl = "http://127.0.0.1:8787",
  [string]$CargoTargetDir = "",
  [switch]$SkipRust
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$repository = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($CargoTargetDir)) {
  $CargoTargetDir = Join-Path $repository ".tmp\cargo-target-real-host-acceptance"
}
$previousTarget = $env:CARGO_TARGET_DIR
$env:CARGO_TARGET_DIR = $CargoTargetDir

function Invoke-Step {
  param(
    [Parameter(Mandatory = $true)][string]$Name,
    [Parameter(Mandatory = $true)][scriptblock]$Action
  )
  Write-Output "STEP $Name"
  & $Action
  if ($LASTEXITCODE -ne 0) { throw "$Name failed with exit status $LASTEXITCODE" }
  Write-Output "PASS $Name exit=0"
}

Push-Location $repository
try {
  if (-not $SkipRust) {
    Invoke-Step "rust-tests" { cargo test --workspace }
  }
  Invoke-Step "frontend-syntax" { npm run check:frontend }
  Invoke-Step "frontend-tests" { npm run test:frontend }
  Invoke-Step "diff-check" { git diff --check }

  $bootstrap = Invoke-RestMethod -Uri "$BaseUrl/api/v1/bootstrap" -TimeoutSec 10
  $world = Invoke-RestMethod -Uri "$BaseUrl/api/v1/views/global/world" -TimeoutSec 10
  $expectedAddresses = @("192.0.2.10", "192.0.2.11", "192.0.2.12")
  $actualAddresses = @($bootstrap.data.hosts | ForEach-Object { $_.address })
  $aliases = @($bootstrap.data.hosts | ForEach-Object { $_.label })
  $fixtureNodes = @($world.data.nodes | Where-Object { $_.state -eq "fixture" })

  if ($bootstrap.meta.data_source.kind -ne "real") { throw "bootstrap data source is not real" }
  if ($bootstrap.data.projects.Count -ne 0) { throw "expected zero projects without SSH scan evidence" }
  if ($bootstrap.data.hosts.Count -ne 3) { throw "expected three registered hosts" }
  foreach ($address in $expectedAddresses) {
    if ($address -notin $actualAddresses) { throw "missing expected host address $address" }
  }
  if ($fixtureNodes.Count -ne 0) { throw "fixture nodes leaked into the real HTTP view" }

  Write-Output "LIVE_API kind=real hosts=3 projects=0 fixture_nodes=0"
  Write-Output "LIVE_API addresses=$($actualAddresses -join ',') aliases=$($aliases -join ',')"
  Write-Output "VERIFY_REAL_HOST_ACCEPTANCE_OK"
} finally {
  Pop-Location
  $env:CARGO_TARGET_DIR = $previousTarget
}
