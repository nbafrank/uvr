# uvr installer for Windows -- https://github.com/nbafrank/uvr
#
# Usage:
#   irm https://raw.githubusercontent.com/nbafrank/uvr/main/install.ps1 | iex
#
# Environment variables:
#   UVR_INSTALL_DIR -- where to place the binary (default: $env:USERPROFILE\.local\bin)
#   UVR_VERSION -- specific version to install (default: latest)

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$Repo = "nbafrank/uvr"
$InstallDir = if ($env:UVR_INSTALL_DIR) { $env:UVR_INSTALL_DIR } else { Join-Path $env:USERPROFILE ".local\bin" }

function Write-Log {
    param([string]$Message)
    Write-Host "uvr-install: $Message"
}

function Write-ErrExit {
    param([string]$Message)
    Write-Host "uvr-install: error: $Message" -ForegroundColor Red
    exit 1
}

function Get-Target {
    $arch = $env:PROCESSOR_ARCHITECTURE
    if ($env:PROCESSOR_ARCHITEW6432) {
        $arch = $env:PROCESSOR_ARCHITEW6432
    }
    switch ($arch) {
        "AMD64" { return "x86_64-pc-windows-msvc" }
        "ARM64" {
            # TODO: switch to a native aarch64-pc-windows-msvc asset once one is published.
            Write-Log "No native ARM64 build published; using the x64 build under emulation."
            return "x86_64-pc-windows-msvc"
        }
        default { Write-ErrExit "Unsupported architecture: $arch" }
    }
}

function Get-LatestVersion {
    $uri = "https://api.github.com/repos/$Repo/releases/latest"
    try {
        $release = Invoke-RestMethod -Uri $uri -Headers @{ "User-Agent" = "uvr-install" }
    } catch {
        Write-ErrExit "Failed to determine latest version: $_"
    }
    if (-not $release.tag_name) {
        Write-ErrExit "Failed to determine latest version"
    }
    return $release.tag_name
}

function Get-ExpectedHash {
    param([string]$BaseUrl, [string]$Asset, [string]$TmpDir)
    $sumsPath = Join-Path $TmpDir "sha256sums.txt"
    try {
        Invoke-WebRequest -Uri "$BaseUrl/sha256sums.txt" -OutFile $sumsPath -UseBasicParsing
    } catch {
        Write-Log "Warning: sha256sums.txt not found, skipping checksum verification"
        return $null
    }
    $line = Select-String -Path $sumsPath -Pattern ([Regex]::Escape($Asset)) | Select-Object -First 1
    if (-not $line) {
        Write-Log "Warning: no checksum entry for $Asset, skipping checksum verification"
        return $null
    }
    return ($line.Line -split "\s+")[0]
}

function Add-ToUserPath {
    param([string]$Dir)
    $key = "HKCU:\Environment"
    $raw = (Get-Item $key).GetValue("Path", "", "DoNotExpandEnvironmentNames")
    $kind = (Get-Item $key).GetValueKind("Path")
    $entries = @()
    if ($raw) {
        $entries = $raw -split ";" | Where-Object { $_ }
    }
    $target = $Dir.TrimEnd("\")
    $alreadyPresent = $entries | Where-Object { $_.TrimEnd("\") -ieq $target }
    if ($alreadyPresent) {
        return
    }
    $newPath = if ($raw) { "$raw;$Dir" } else { $Dir }
    Set-ItemProperty -Path $key -Name Path -Value $newPath -Type $kind
    Write-Log "Added $Dir to your user PATH."
    Write-Log "Open a new terminal for the change to take effect there."
}

function main {
    $target = Get-Target
    Write-Log "Detected platform: $target"

    $version = if ($env:UVR_VERSION) { $env:UVR_VERSION } else { Get-LatestVersion }
    Write-Log "Installing uvr $version"

    $asset = "uvr-$target.zip"
    $baseUrl = "https://github.com/$Repo/releases/download/$version"
    $tmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
    New-Item -ItemType Directory -Path $tmpDir | Out-Null

    try {
        $zipPath = Join-Path $tmpDir $asset
        Write-Log "Downloading $asset..."
        try {
            Invoke-WebRequest -Uri "$baseUrl/$asset" -OutFile $zipPath -UseBasicParsing
        } catch {
            Write-ErrExit "Download failed: $_"
        }

        $expected = Get-ExpectedHash -BaseUrl $baseUrl -Asset $asset -TmpDir $tmpDir
        if ($expected) {
            Write-Log "Verifying checksum..."
            $actual = (Get-FileHash -Path $zipPath -Algorithm SHA256).Hash.ToLower()
            if ($expected.ToLower() -ne $actual) {
                Write-ErrExit "Checksum mismatch!`n  Expected: $expected`n  Got: $actual"
            }
            Write-Log "Checksum verified"
        }

        Write-Log "Extracting..."
        Expand-Archive -Path $zipPath -DestinationPath $tmpDir -Force

        $exePath = Get-ChildItem -Path $tmpDir -Filter "uvr.exe" -Recurse | Select-Object -First 1
        if (-not $exePath) {
            Write-ErrExit "uvr.exe not found in downloaded archive"
        }

        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        Copy-Item -Path $exePath.FullName -Destination (Join-Path $InstallDir "uvr.exe") -Force
    } finally {
        Remove-Item -Path $tmpDir -Recurse -Force -ErrorAction SilentlyContinue
    }

    Write-Log "Installed uvr to $InstallDir\uvr.exe"

    Add-ToUserPath -Dir $InstallDir
    $env:Path = "$env:Path;$InstallDir"

    & (Join-Path $InstallDir "uvr.exe") --version
}

main
