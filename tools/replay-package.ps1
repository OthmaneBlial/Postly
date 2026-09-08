[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Archive,

    [switch]$SkipGui
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$archivePath = (Resolve-Path -LiteralPath $Archive -ErrorAction Stop).Path
$root = Join-Path ([IO.Path]::GetTempPath()) ("postly-package-replay-" + [Guid]::NewGuid().ToString("N"))
$extract = Join-Path $root "extracted"
$demoProcess = $null
$guiProcess = $null

function Stop-ChildProcess {
    param([System.Diagnostics.Process]$Process)

    if ($null -ne $Process -and -not $Process.HasExited) {
        Stop-Process -Id $Process.Id -Force -ErrorAction SilentlyContinue
        $Process.WaitForExit(3000)
    }
}

function Assert-Equal {
    param(
        [string]$Actual,
        [string]$Expected,
        [string]$Message
    )

    if ($Actual -ne $Expected) {
        throw "$Message (expected '$Expected', got '$Actual')"
    }
}

function Test-Checksums {
    param([string]$Package)

    $manifestPath = Join-Path $Package "SHA256SUMS"
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw "package does not contain SHA256SUMS"
    }

    $checked = 0
    foreach ($line in Get-Content -LiteralPath $manifestPath) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        if ($line -notmatch '^([0-9a-fA-F]{64})\s{2}(.+)$') {
            throw "invalid checksum line: $line"
        }
        $expected = $Matches[1].ToLowerInvariant()
        $relative = $Matches[2].Replace('/', [IO.Path]::DirectorySeparatorChar)
        $file = Join-Path $Package $relative
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
            throw "checksum entry is missing: $relative"
        }
        $actual = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()
        Assert-Equal $actual $expected "checksum mismatch for $relative"
        $checked++
    }
    if ($checked -eq 0) {
        throw "SHA256SUMS is empty"
    }
    return $checked
}

try {
    New-Item -ItemType Directory -Path $extract -Force | Out-Null
    Expand-Archive -LiteralPath $archivePath -DestinationPath $extract -Force

    $directories = @(Get-ChildItem -LiteralPath $extract -Directory)
    if ($directories.Count -ne 1) {
        throw "archive must contain exactly one top-level package directory"
    }
    $package = $directories[0].FullName
    $cli = Join-Path $package "postly.exe"
    $gui = Join-Path $package "postly-gui.exe"
    foreach ($binary in @($cli, $gui)) {
        if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
            throw "package is missing $([IO.Path]::GetFileName($binary))"
        }
    }

    $manifest = Get-Content -LiteralPath (Join-Path $package "postly-package.json") -Raw | ConvertFrom-Json
    $version = [string]$manifest.version
    if ([string]::IsNullOrWhiteSpace($version)) {
        throw "package manifest does not contain a version"
    }
    $checked = Test-Checksums $package

    $versionOutput = (& $cli --version | Out-String).Trim()
    Assert-Equal $versionOutput "postly $version" "unexpected CLI version"
    & $cli --help | Out-Null

    $workspace = Join-Path $root "orders"
    $demoStdout = Join-Path $root "demo.stdout.log"
    $demoStderr = Join-Path $root "demo.stderr.log"
    $quotedWorkspace = '"' + $workspace.Replace('"', '\"') + '"'
    $demoProcess = Start-Process -FilePath $cli -ArgumentList @("demo", $quotedWorkspace, "--port", "0") -RedirectStandardOutput $demoStdout -RedirectStandardError $demoStderr -PassThru
    $ready = $false
    for ($attempt = 0; $attempt -lt 100; $attempt++) {
        if (Test-Path -LiteralPath (Join-Path $workspace "postly.toml") -PathType Leaf) {
            $ready = $true
            break
        }
        if ($demoProcess.HasExited) {
            throw "packaged demo exited before creating its workspace`n$(Get-Content -LiteralPath $demoStderr -Raw)"
        }
        Start-Sleep -Milliseconds 100
    }
    if (-not $ready) {
        throw "packaged demo workspace was not created within 10 seconds"
    }

    $validationJson = (& $cli validate $workspace --output-json | Out-String)
    $validation = $validationJson | ConvertFrom-Json
    if (-not [bool]$validation.valid) {
        throw "packaged workspace validation failed"
    }
    $runJson = (& $cli run $workspace --reporter json | Out-String)
    $report = ($runJson | ConvertFrom-Json)[0]
    Assert-Equal ([string]$report.passed) "2" "packaged demo request count did not pass"
    Assert-Equal ([string]$report.failed) "0" "packaged demo reported a failure"
    Assert-Equal ([string]$report.assertions) "5" "packaged demo assertion count changed"

    Stop-ChildProcess $demoProcess
    $demoProcess = $null

    if (-not $SkipGui) {
        $guiStdout = Join-Path $root "gui.stdout.log"
        $guiStderr = Join-Path $root "gui.stderr.log"
        $guiProcess = Start-Process -FilePath $gui -ArgumentList @($quotedWorkspace) -RedirectStandardOutput $guiStdout -RedirectStandardError $guiStderr -PassThru
        Start-Sleep -Seconds 3
        if ($guiProcess.HasExited) {
            throw "packaged GUI exited before the three-second smoke window`n$(Get-Content -LiteralPath $guiStderr -Raw)"
        }
        Stop-ChildProcess $guiProcess
        $guiProcess = $null
    }

    Write-Output "package replay: PASS"
    Write-Output "archive: $archivePath"
    Write-Output "version: $version"
    Write-Output "checksums ($checked), CLI version/help, loopback demo, validation and run passed"
    if ($SkipGui) {
        Write-Output "GUI smoke: SKIPPED (-SkipGui)"
    } else {
        Write-Output "GUI smoke: process stayed alive for three seconds"
    }
}
finally {
    Stop-ChildProcess $guiProcess
    Stop-ChildProcess $demoProcess
    if (Test-Path -LiteralPath $root) {
        Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
    }
}
