[CmdletBinding()]
param(
  [int]$AppPort = 18798,
  [int]$HttpsPort = 18443,
  [string]$CaddyPath = "target/m4-tools/release/caddy.exe",
  [string]$BrowserRecordPath = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($BrowserRecordPath)) {
  $BrowserRecordPath = Join-Path $repoRoot "artifacts/m4-remote-release-20260811/BROWSER-VERIFICATION.json"
} else {
  $BrowserRecordPath = (Resolve-Path -LiteralPath $BrowserRecordPath).Path
}

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

function Assert-Equal {
  param(
    [Parameter(Mandatory = $true)]$Actual,
    [Parameter(Mandatory = $true)]$Expected,
    [Parameter(Mandatory = $true)][string]$Label
  )
  if ($Actual -ne $Expected) {
    throw "$Label expected '$Expected' but got '$Actual'"
  }
}

function Get-Operation {
  param(
    [Parameter(Mandatory = $true)]$OpenApi,
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$Method
  )
  $pathProperty = $OpenApi.paths.PSObject.Properties[$Path]
  if (-not $pathProperty) { throw "OpenAPI path is missing: $Path" }
  $operationProperty = $pathProperty.Value.PSObject.Properties[$Method]
  if (-not $operationProperty) { throw "OpenAPI operation is missing: $Method $Path" }
  return $operationProperty.Value
}

function Assert-RequiredHeader {
  param(
    [Parameter(Mandatory = $true)]$Operation,
    [Parameter(Mandatory = $true)][string]$Name,
    [Parameter(Mandatory = $true)][string]$Label
  )
  $headers = @($Operation.parameters | Where-Object { $_.name -eq $Name -and $_.in -eq "header" })
  if ($headers.Count -ne 1 -or -not $headers[0].required) {
    throw "$Label must require $Name"
  }
}

Push-Location $repoRoot
try {
  Invoke-NativeStep "rust-format" "cargo" "fmt" "--all" "--" "--check"
  Invoke-NativeStep "rust-check" "cargo" "check" "--workspace"
  Invoke-NativeStep "rust-build" "cargo" "build" "--workspace" "--examples"
  Invoke-NativeStep "rust-tests-m0-m4" "cargo" "test" "--workspace"
  Invoke-NativeStep "rust-tests-m4-boundaries" "cargo" "test" "--test" "m4_remote_release"
  Invoke-NativeStep "rust-clippy" "cargo" "clippy" "--workspace" "--all-targets" "--" "-D" "warnings"
  Invoke-NativeStep "openapi-export" "cargo" "run" "-p" "network-atlas" "--" "--export-openapi" "openapi/openapi.json"
  Invoke-NativeStep "frontend-types" "npm" "run" "generate:types"
  Invoke-NativeStep "frontend-check" "npm" "run" "check:frontend"
  Invoke-NativeStep "frontend-tests" "npm" "run" "test:frontend"

  $openApi = Get-Content -LiteralPath "openapi/openapi.json" -Raw -Encoding utf8 | ConvertFrom-Json
  $scheme = $openApi.components.securitySchemes.owner_session
  Assert-Equal $scheme.type "apiKey" "owner session security type"
  Assert-Equal $scheme.in "cookie" "owner session security location"
  Assert-Equal $scheme.name "network_atlas_session" "owner session cookie name"
  if ((Get-Operation $openApi "/api/v1/auth/login" "post").PSObject.Properties["security"]) {
    throw "Login must remain public in OpenAPI"
  }

  $protectedOperations = 0
  foreach ($pathProperty in $openApi.paths.PSObject.Properties) {
    if (-not $pathProperty.Name.StartsWith("/api/v1/") -or $pathProperty.Name -eq "/api/v1/auth/login") {
      continue
    }
    foreach ($method in @("get", "post", "put", "patch", "delete")) {
      $operationProperty = $pathProperty.Value.PSObject.Properties[$method]
      if (-not $operationProperty) { continue }
      $ownerRequirement = @($operationProperty.Value.security | Where-Object {
          $_.PSObject.Properties["owner_session"]
        })
      if ($ownerRequirement.Count -ne 1) {
        throw "$method $($pathProperty.Name) does not declare owner_session"
      }
      $protectedOperations += 1
    }
  }
  if ($protectedOperations -lt 25) { throw "Protected OpenAPI surface is unexpectedly small: $protectedOperations" }

  $m4Operations = @(
    @{ path = "/api/v1/auth/login"; method = "post" },
    @{ path = "/api/v1/auth/session"; method = "get" },
    @{ path = "/api/v1/auth/logout"; method = "post" },
    @{ path = "/api/v1/events/stream"; method = "get" },
    @{ path = "/api/v1/exports/workspace"; method = "get" },
    @{ path = "/api/v1/hosts/{host_id}/export"; method = "get" },
    @{ path = "/api/v1/projects/{project_id}/export"; method = "get" },
    @{ path = "/api/v1/workspace"; method = "delete" },
    @{ path = "/api/v1/hosts/{host_id}"; method = "delete" },
    @{ path = "/api/v1/projects/{project_id}"; method = "delete" }
  )
  foreach ($entry in $m4Operations) {
    Get-Operation $openApi $entry.path $entry.method | Out-Null
  }
  foreach ($entry in $m4Operations | Where-Object { $_.method -eq "delete" }) {
    $operation = Get-Operation $openApi $entry.path $entry.method
    Assert-RequiredHeader $operation "Idempotency-Key" "$($entry.method) $($entry.path)"
    Assert-RequiredHeader $operation "X-Confirm-Delete" "$($entry.method) $($entry.path)"
  }
  $modelFields = @($openApi.components.schemas.ModelProviderData.properties.PSObject.Properties.Name)
  foreach ($forbidden in @("api_key", "credential_ref")) {
    if ($modelFields -contains $forbidden) { throw "ModelProviderData exposes $forbidden" }
  }
  Write-Output "PASS openapi-m4-contract protected=$protectedOperations m4-routes=$($m4Operations.Count) cookie=declared destructive-headers=required"

  $backendContractFiles = @(Get-ChildItem -LiteralPath "docs" -Filter "09-Rust*.md" -File)
  Assert-Equal $backendContractFiles.Count 1 "backend contract document count"
  $backendContract = Get-Content -LiteralPath $backendContractFiles[0].FullName -Raw -Encoding utf8
  $documentedPaths = 0
  foreach ($pathProperty in $openApi.paths.PSObject.Properties) {
    if (-not $backendContract.Contains($pathProperty.Name)) {
      throw "Backend contract document is missing OpenAPI path: $($pathProperty.Name)"
    }
    $documentedPaths += 1
  }
  Write-Output "PASS docs-openapi-contract paths=$documentedPaths"

  $vendorHash = (Get-FileHash -Algorithm SHA256 -LiteralPath "frontend/vendor/lucide-0.468.0.min.js").Hash.ToLowerInvariant()
  Assert-Equal $vendorHash "3411692820cb8d47543f69496aa25fd603a358f4498046f41c508a5a3342210e" "vendored Lucide SHA-256"
  Write-Output "PASS frontend-vendor version=0.468.0 sha256=$vendorHash"

  if (-not (Test-Path -LiteralPath $browserRecordPath -PathType Leaf)) {
    throw "Browser verification record is missing"
  }
  $browserRecord = Get-Content -LiteralPath $browserRecordPath -Raw -Encoding utf8 | ConvertFrom-Json
  foreach ($checkName in @("desktop_dashboard", "desktop_owner_data", "mobile_dashboard", "mobile_owner_data", "mobile_logout_login")) {
    $check = $browserRecord.checks.PSObject.Properties[$checkName].Value
    foreach ($property in @("outside_control_count", "button_text_overflow_count", "control_text_overflow_count", "console_warning_or_error_count")) {
      $candidate = $check.PSObject.Properties[$property]
      if ($candidate) { Assert-Equal $candidate.Value 0 "browser $checkName $property" }
    }
  }
  Assert-Equal $browserRecord.checks.mobile_dashboard.document_client_width $browserRecord.checks.mobile_dashboard.document_scroll_width "mobile document width"
  Assert-Equal $browserRecord.checks.mobile_logout_login.document_client_width $browserRecord.checks.mobile_logout_login.document_scroll_width "mobile login document width"
  foreach ($screenshot in $browserRecord.screenshots) {
    $path = Join-Path (Split-Path -Parent $browserRecordPath) ("screenshots/" + $screenshot.name)
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Browser screenshot is missing: $($screenshot.name)" }
    $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
    Assert-Equal $actualHash $screenshot.sha256 "browser screenshot $($screenshot.name)"
  }
  Write-Output "PASS browser-evidence screenshots=$(@($browserRecord.screenshots).Count) desktop=checked mobile=390x844 geometry=checked logs=clean"

  $bashScripts = @(Get-ChildItem -LiteralPath "deploy/scripts" -Filter "*.sh" -File | Sort-Object Name)
  Assert-Equal $bashScripts.Count 6 "deployment script count"
  foreach ($script in $bashScripts) {
    Invoke-NativeStep ("bash-syntax-" + $script.BaseName) "bash" "-n" ("deploy/scripts/" + $script.Name)
  }
  Invoke-NativeStep "deploy-command-flow" "bash" "scripts/verify-m4-deploy.sh"
  Invoke-NativeStep "compose-config" "docker" "compose" "--env-file" "deploy/.env.example" "-f" "deploy/compose.yaml" "config" "--quiet"

  $runtimeBefore = @(Get-ChildItem -LiteralPath "target" -Directory -Filter "m4-https-*" -ErrorAction SilentlyContinue | ForEach-Object FullName)
  $passwordSentinel = "m4-verify-$([Guid]::NewGuid().ToString('N'))"
  Invoke-NativeStep "https-smoke" "powershell" "-NoProfile" "-ExecutionPolicy" "Bypass" "-File" "scripts/verify-m4-https.ps1" `
    "-AppPort" ([string]$AppPort) "-HttpsPort" ([string]$HttpsPort) "-CaddyPath" $CaddyPath "-Password" $passwordSentinel
  $runtimeAfter = @(Get-ChildItem -LiteralPath "target" -Directory -Filter "m4-https-*" -ErrorAction SilentlyContinue | ForEach-Object FullName)
  $newRuntime = @($runtimeAfter | Where-Object { $runtimeBefore -notcontains $_ })
  Assert-Equal $newRuntime.Count 0 "HTTPS temporary runtime cleanup"

  $scanRoots = @("backend", "frontend", "openapi", "deploy", "scripts", "README.md", "work/MVP-1-PROGRESS.md")
  $scanFiles = foreach ($root in $scanRoots) {
    if (Test-Path -LiteralPath $root -PathType Leaf) { Get-Item -LiteralPath $root }
    elseif (Test-Path -LiteralPath $root -PathType Container) { Get-ChildItem -LiteralPath $root -Recurse -File }
  }
  foreach ($file in $scanFiles | Where-Object { $_.Length -lt 5MB -and $_.Extension -notin @(".png", ".zip", ".exe", ".db") }) {
    $content = [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($file.FullName))
    if ($content.Contains($passwordSentinel)) { throw "HTTPS password sentinel leaked to $($file.FullName)" }
  }
  Write-Output "PASS m4-secret-leak-scan sentinel=absent runtime=cleaned"
  Write-Output "VERIFY_M4_OK"
} finally {
  Pop-Location
}
