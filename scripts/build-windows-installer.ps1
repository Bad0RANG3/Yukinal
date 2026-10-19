param(
  [string]$BundleDirectory = "target/release/bundle",
  [string]$OutputDirectory = "target/release/installers"
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$bundle = (Resolve-Path (Join-Path $repo $BundleDirectory)).Path
$manifestPath = Join-Path $bundle 'release-manifest.json'
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
  throw "Release manifest not found: $manifestPath"
}
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
if ($manifest.application -ne 'yukinal' -or $manifest.platform -ne 'win32') {
  throw "Expected a Windows Yukinal payload, got $($manifest.application) / $($manifest.platform)."
}
if ($manifest.dirty -ne $false) {
  throw 'Refusing to build a Windows installer from a dirty source tree.'
}

$payload = Join-Path $bundle $manifest.payloadDirectory
foreach ($required in @(
    'yukinal-desktop-host.exe',
    'yukinal.cmd',
    'ui/index.html',
    'python-agent/yukinal-agent.exe',
    'ssh-service/yukinal-ssh-service.exe')) {
  $path = Join-Path $payload $required
  if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Required payload file is missing: $path" }
}

$makensis = Get-Command 'makensis.exe' -ErrorAction Stop
$iconOutput = Join-Path $payload 'assets/yukinal.ico'
if (-not (Test-Path -LiteralPath $iconOutput -PathType Leaf)) { throw "Windows application icon is missing: $iconOutput" }

$output = Join-Path $repo $OutputDirectory
New-Item -ItemType Directory -Path $output -Force | Out-Null
$architecture = switch ($manifest.arch) {
  'x64' { 'x64' }
  'arm64' { 'arm64' }
  default { throw "Unsupported Windows release architecture: $($manifest.arch)" }
}
$installer = Join-Path $output "yukinal-$($manifest.version)-windows-$architecture-setup.exe"
$script = Join-Path $repo 'packaging/windows/yukinal.nsi'

$arguments = @(
  "/DAPP_VERSION=$($manifest.version)",
  "/DPAYLOAD_DIR=$payload",
  "/DOUT_FILE=$installer",
  "/DICON_FILE=$iconOutput",
  $script
)
& $makensis.Source @arguments
if ($LASTEXITCODE -ne 0) { throw "NSIS failed with exit code $LASTEXITCODE." }
if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) { throw "NSIS did not create $installer" }

Write-Output "Windows installer: $installer"
