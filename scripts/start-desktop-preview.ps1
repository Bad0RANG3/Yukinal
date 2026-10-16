param([switch]$CheckOnly)

$ErrorActionPreference = 'Stop'
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..')).TrimEnd('\')
$hostBinary = Join-Path $repoRoot 'native\build-cpp-host\yukinal-desktop-host.exe'
$buildDirectory = Join-Path $repoRoot 'native\build-cpp-host'
$viteEntry = Join-Path $repoRoot 'apps\desktop\node_modules\vite\bin\vite.js'
$profileDirectory = Join-Path $repoRoot 'target\desktop-preview-profile'
$logDirectory = Join-Path $repoRoot 'target\desktop-preview-logs'

function Get-PreviewListener([int]$Port) {
  return @(Get-NetTCPConnection -State Listen -LocalPort $Port -ErrorAction SilentlyContinue)
}

function Test-OwnVite($Listener) {
  $owner = Get-CimInstance Win32_Process -Filter "ProcessId=$($Listener.OwningProcess)" -ErrorAction SilentlyContinue
  if (-not $owner -or $owner.Name -ine 'node.exe' -or -not $owner.CommandLine) { return $false }
  $command = $owner.CommandLine.Replace('/', '\')
  return $command.IndexOf(($repoRoot + '\'), [StringComparison]::OrdinalIgnoreCase) -ge 0 -and
    $command -match '\\vite\\bin\\vite\.js["\s]'
}

try {
  if (-not (Test-Path -LiteralPath $viteEntry -PathType Leaf)) {
    throw 'Dependencies are missing. Run pnpm install --frozen-lockfile in this checkout first.'
  }
  $nodeBinary = (Get-Command node.exe -ErrorAction Stop).Source
  $cmakeCommand = Get-Command cmake.exe -ErrorAction SilentlyContinue
  $canBuild = $cmakeCommand -and (Test-Path -LiteralPath (Join-Path $buildDirectory 'CMakeCache.txt'))
  if (-not (Test-Path -LiteralPath $hostBinary -PathType Leaf) -and -not $canBuild) {
    throw 'The desktop host has not been built. Run pnpm desktop:dev:cpp once in this checkout.'
  }

  $oldInstances = @(Get-CimInstance Win32_Process -Filter "Name='yukinal-desktop-host.exe'" |
    Where-Object { $_.ExecutablePath -and [IO.Path]::GetFullPath($_.ExecutablePath) -ieq $hostBinary })
  $previewPort = 0
  $oldViteInstances = @()
  foreach ($candidatePort in 1422..1442) {
    $listeners = @(Get-PreviewListener $candidatePort)
    if ($listeners.Count -eq 0) { $previewPort = $candidatePort; break }
    if (@($listeners | Where-Object { -not (Test-OwnVite $_) }).Count -eq 0) {
      $previewPort = $candidatePort
      $oldViteInstances = @($listeners | Select-Object -ExpandProperty OwningProcess -Unique |
        ForEach-Object { Get-CimInstance Win32_Process -Filter "ProcessId=$_" })
      break
    }
  }
  if (-not $previewPort) { throw 'No preview port is available between 1422 and 1442.' }
  $previewUrl = "http://127.0.0.1:$previewPort/?fixture=1"

  if ($CheckOnly) {
    [pscustomobject]@{
      HostBinary = $hostBinary
      RestartProcessIds = @($oldInstances | ForEach-Object { $_.ProcessId })
      PreviewUrl = $previewUrl
      RestartPreviewProcessIds = @($oldViteInstances | ForEach-Object { $_.ProcessId })
      RebuildHost = [bool]$canBuild
      DataDirectory = $profileDirectory
    } | ConvertTo-Json
    exit 0
  }

  foreach ($instance in @($oldInstances) + @($oldViteInstances)) {
    $liveInstance = Get-CimInstance Win32_Process -Filter "ProcessId=$($instance.ProcessId)" -ErrorAction SilentlyContinue
    if (-not $liveInstance -or $liveInstance.CreationDate -ne $instance.CreationDate) { continue }
    Write-Host 'Closing the previous Yukinal instance...'
    & "$env:SystemRoot\System32\taskkill.exe" /PID $instance.ProcessId /T /F | Out-Null
    if ($LASTEXITCODE -ne 0 -and (Get-Process -Id $instance.ProcessId -ErrorAction SilentlyContinue)) {
      throw "Could not close Yukinal process $($instance.ProcessId)."
    }
  }

  New-Item -ItemType Directory -Path $profileDirectory, $logDirectory -Force | Out-Null
  if ($canBuild) {
    Write-Host 'Updating Yukinal...'
    & $cmakeCommand.Source --build $buildDirectory --target yukinal-desktop-host -j 4
    if ($LASTEXITCODE -ne 0) { throw 'The desktop build failed.' }
  }

  Write-Host 'Starting the preview service...'
  $viteProcess = Start-Process -FilePath $nodeBinary -WorkingDirectory (Join-Path $repoRoot 'apps\desktop') `
    -ArgumentList @(('"{0}"' -f $viteEntry), '--host', '127.0.0.1', '--port', $previewPort, '--strictPort') `
    -WindowStyle Hidden -PassThru `
    -RedirectStandardOutput (Join-Path $logDirectory 'vite-output.log') `
    -RedirectStandardError (Join-Path $logDirectory 'vite-error.log')
  $ready = $false
  $deadline = (Get-Date).AddSeconds(30)
  while ((Get-Date) -lt $deadline) {
    if ($viteProcess -and $viteProcess.HasExited) { throw "The preview service exited. See $logDirectory" }
    try {
      $response = Invoke-WebRequest -Uri $previewUrl -UseBasicParsing -TimeoutSec 2
      if ($response.StatusCode -eq 200) { $ready = $true; break }
    } catch { }
    Start-Sleep -Milliseconds 200
  }
  if (-not $ready) { throw "The preview service did not become ready. See $logDirectory" }

  if (-not $env:YUKINAL_AGENT_PYTHON) {
    $pythonCandidates = @(Join-Path $repoRoot 'agent\.venv\Scripts\python.exe')
    if (Get-Command git.exe -ErrorAction SilentlyContinue) {
      $gitDirectory = & git.exe -C $repoRoot rev-parse --path-format=absolute --git-common-dir 2>$null
      if ($LASTEXITCODE -eq 0 -and $gitDirectory) {
        $pythonCandidates += Join-Path (Split-Path $gitDirectory -Parent) 'agent\.venv\Scripts\python.exe'
      }
    }
    foreach ($candidate in $pythonCandidates) {
      if (Test-Path -LiteralPath $candidate -PathType Leaf) { $env:YUKINAL_AGENT_PYTHON = $candidate; break }
    }
  }
  $env:YUKINAL_AGENT_SOURCE = Join-Path $repoRoot 'agent\src'
  Write-Host 'Opening Yukinal with mock data...'
  $desktopProcess = Start-Process -FilePath $hostBinary -WorkingDirectory $repoRoot `
    -ArgumentList @('--dev-url', $previewUrl, '--data-dir', ('"{0}"' -f $profileDirectory)) `
    -WindowStyle Normal -PassThru
  $deadline = (Get-Date).AddSeconds(15)
  do {
    if ($desktopProcess.HasExited) { throw "Yukinal exited with code $($desktopProcess.ExitCode)." }
    $desktopProcess.Refresh()
    if ($desktopProcess.MainWindowHandle -ne 0) { Write-Host 'Yukinal is running.'; exit 0 }
    Start-Sleep -Milliseconds 200
  } while ((Get-Date) -lt $deadline)
  throw 'Yukinal did not open a window within 15 seconds.'
} catch {
  Write-Host $_.Exception.Message -ForegroundColor Red
  exit 1
}
