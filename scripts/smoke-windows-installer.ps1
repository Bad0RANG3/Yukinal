param(
  [Parameter(Mandatory = $true)]
  [string]$Installer,
  [string]$PayloadDirectory = "target/release/bundle"
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$installerPath = (Resolve-Path $Installer).Path
$runId = [Guid]::NewGuid().ToString('N')
$runRoot = Join-Path $repo "target/release/installer-smoke/windows/$runId"
$installDirectory = Join-Path $runRoot 'install'
$dataDirectory = Join-Path $runRoot 'user-data'
$marker = Join-Path $dataDirectory 'must-survive-uninstall.txt'
New-Item -ItemType Directory -Path $runRoot, $dataDirectory -Force | Out-Null
Set-Content -LiteralPath $marker -Value 'Yukinal keeps user data outside the installation folder.' -NoNewline

if ($installDirectory -match '[\s"]') {
  throw "NSIS /D= smoke paths must not contain spaces or quotes: $installDirectory"
}

# NSIS requires /D= to be the final, unquoted argument.
$setup = Start-Process -FilePath $installerPath -ArgumentList @('/S', "/D=$installDirectory") -Wait -PassThru
if ($setup.ExitCode -ne 0) { throw "NSIS installation failed with exit code $($setup.ExitCode)." }

$hostExe = Join-Path $installDirectory 'yukinal-desktop-host.exe'
$launcher = Join-Path $installDirectory 'yukinal.cmd'
$agent = Join-Path $installDirectory 'python-agent/yukinal-agent.exe'
$uninstaller = Join-Path $installDirectory 'Uninstall.exe'
foreach ($path in @($hostExe, $launcher, $agent, $uninstaller, (Join-Path $installDirectory 'ui/index.html'))) {
  if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Installed file is missing: $path" }
}

$originalDataDirectory = $env:YUKINAL_DATA_DIR
$env:YUKINAL_DATA_DIR = $dataDirectory
$hostCheck = Start-Process -FilePath $hostExe -ArgumentList @('--self-check') -Wait -PassThru -NoNewWindow
if ($hostCheck.ExitCode -ne 0) { throw "Installed desktop host self-check failed with exit code $($hostCheck.ExitCode)." }

$launch = & $launcher --help 2>&1
if ($LASTEXITCODE -ne 0) { throw "Installed launcher failed with exit code $LASTEXITCODE`: $launch" }

$node = Get-Command 'node.exe' -ErrorAction Stop
$originalPath = $env:Path
try {
  # Run the frozen Agent with a system-only PATH. Node is used only to execute this
  # smoke harness; the Agent process cannot discover a machine-wide Python or Node.
  $env:Path = "$env:SystemRoot\System32;$env:SystemRoot"
  $env:YUKINAL_DATA_DIR = $dataDirectory
  & $node.Source (Join-Path $repo 'scripts/smoke-python-agent.mjs') --executable $agent
  if ($LASTEXITCODE -ne 0) { throw "The installed Python Agent smoke failed with exit code $LASTEXITCODE." }
} finally {
  $env:Path = $originalPath
}

$uninstall = Start-Process -FilePath $uninstaller -ArgumentList @('/S') -Wait -PassThru
if ($uninstall.ExitCode -ne 0) { throw "NSIS uninstall failed with exit code $($uninstall.ExitCode)." }
if (Test-Path -LiteralPath $installDirectory) { throw "NSIS left the isolated installation directory behind: $installDirectory" }
if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
  throw 'The isolated user-data marker was removed by uninstall.'
}
if ($null -eq $originalDataDirectory) {
  Remove-Item Env:YUKINAL_DATA_DIR -ErrorAction SilentlyContinue
} else {
  $env:YUKINAL_DATA_DIR = $originalDataDirectory
}

Write-Output "PASS NSIS install, packaged host and Agent startup, uninstall, and user-data retention ($runRoot)"
