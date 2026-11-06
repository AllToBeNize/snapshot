[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('help', 'build', 'test', 'package', 'release')]
    [string]$Command = 'help',
    [string]$Tag
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$projectRoot = $PSScriptRoot
$target = 'x86_64-pc-windows-msvc'

function Invoke-Checked {
    param([string]$Program, [string[]]$Arguments)
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed (exit $LASTEXITCODE)." }
}

function Initialize-BuildTools {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
        if (Test-Path (Join-Path $cargoBin 'cargo.exe')) {
            $env:PATH = "$cargoBin;$env:PATH"
        } else { throw 'Install Rust (MSVC toolchain) from https://rustup.rs first.' }
    }
    if (-not (Get-Command link.exe -ErrorAction SilentlyContinue)) {
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
        if (-not (Test-Path $vswhere)) { throw 'Install Visual Studio C++ build tools and Windows SDK first.' }
        $vsPath = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if (-not $vsPath) { throw 'Visual Studio x64 C++ build tools were not found.' }
        & (Join-Path $vsPath 'Common7\Tools\Launch-VsDevShell.ps1') -Arch amd64 -HostArch amd64 -SkipAutomaticLocation | Out-Host
    }
    Invoke-Checked rustup @('target', 'add', $target)
}

if ($Command -eq 'help') {
    @'
Snapshot build CLI (Windows x64)
  .\snapshot.ps1 build     Build the release executable
  .\snapshot.ps1 test      Run release tests
  .\snapshot.ps1 package   Test, build, and create dist ZIP + SHA256
  .\snapshot.ps1 release -Tag v0.1.0
                          Package and upload a GitHub Release draft
Release requires gh auth login, a clean checkout, and an origin tag at HEAD.
The tag must match the version in Cargo.toml. Publish the draft on GitHub.
'@ | Write-Host
    exit 0
}

Push-Location $projectRoot
try {
    $manifest = Get-Content Cargo.toml -Raw
    $version = [regex]::Match($manifest, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
    if (-not $version) { throw 'Cannot read package version from Cargo.toml.' }
    if ($Tag -and $Command -ne 'release') { throw '-Tag is only supported by release.' }
    if ($Command -eq 'release') {
        if ($Tag -cne "v$version") { throw "Use -Tag v$version (Cargo.toml version)." }
        if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { throw 'Install GitHub CLI and run gh auth login first.' }
        Invoke-Checked gh @('auth', 'status')
        $status = & git status --porcelain
        if ($LASTEXITCODE -ne 0) { throw 'Cannot read git status.' }
        if ($status) { throw 'Commit changes before creating a release.' }
        $head = & git rev-parse HEAD
        if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve HEAD.' }
        $remoteTag = & git ls-remote origin "refs/tags/$Tag" "refs/tags/$Tag^{}"
        if ($LASTEXITCODE -ne 0) { throw 'Cannot read origin tags.' }
        if (-not $remoteTag) { throw "Push tag $Tag to origin first." }
        $resolvedTag = @($remoteTag)[-1] -split '\s+'
        if ($resolvedTag[0] -ne $head) { throw 'Origin tag must point to the current HEAD.' }
    }
    Initialize-BuildTools
    if ($Command -in @('test', 'package', 'release')) {
        Invoke-Checked cargo @('test', '--release', '--locked', '--target', $target, '--target-dir', 'target')
    }
    if ($Command -ne 'test') {
        Invoke-Checked cargo @('build', '--release', '--locked', '--target', $target, '--target-dir', 'target')
    }
    if ($Command -in @('package', 'release')) {
        $dist = Join-Path $projectRoot 'dist'
        New-Item -ItemType Directory -Path $dist -Force | Out-Null
        $zip = Join-Path $dist "snapshot-v$version-windows-x64.zip"
        $staging = Join-Path ([IO.Path]::GetTempPath()) ('snapshot-package-' + [guid]::NewGuid())
        New-Item -ItemType Directory -Path $staging | Out-Null
        try {
            Copy-Item "target\$target\release\snapshot.exe", 'README.md', 'LICENSE' -Destination $staging
            Copy-Item licenses -Destination $staging -Recurse
            Compress-Archive -Path (Join-Path $staging '*') -DestinationPath $zip -Force
        } finally {
            $resolvedStaging = [IO.Path]::GetFullPath($staging)
            $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
            if (-not $resolvedStaging.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) {
                throw 'Refusing to remove a staging directory outside TEMP.'
            }
            Remove-Item -LiteralPath $resolvedStaging -Recurse
        }
        $checksum = "$zip.sha256"
        $hash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
        [IO.File]::WriteAllText($checksum, "$hash  $([IO.Path]::GetFileName($zip))`n", [Text.UTF8Encoding]::new($false))
        Write-Host "Package: $zip"
        Write-Host "SHA256:  $checksum"
        if ($Command -eq 'release') {
            Invoke-Checked gh @('release', 'create', $Tag, $zip, $checksum, '--verify-tag', '--draft', '--title', "Snapshot $Tag", '--generate-notes')
        }
    }
} catch {
    Write-Error $_ -ErrorAction Continue
    exit 1
} finally { Pop-Location }
