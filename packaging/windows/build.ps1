<#
.SYNOPSIS
    Build the Windows installer.

.DESCRIPTION
    Compiles a release binary, checks it carries nothing about the machine
    that built it, turns LICENSE into the RTF the installer UI wants, and
    hands both to WiX.

    Needs the WiX toolset, which is a dotnet tool:
        dotnet tool install --global wix --version 5.0.2

.EXAMPLE
    .\packaging\windows\build.ps1
    .\packaging\windows\build.ps1 -Version 0.1.0 -SkipBuild
#>
[CmdletBinding()]
param(
    # Defaults to the workspace version in Cargo.toml.
    [string] $Version,
    # Package the release binary that is already built. It is still checked.
    [switch] $SkipBuild
)

$ErrorActionPreference = 'Stop'

# Run a native command without its stderr being mistaken for a failure.
# PowerShell turns a native program's stderr into error records, and under
# ErrorActionPreference = Stop that aborts the script even when the program
# succeeded -- cargo and wix both write progress there. Only the exit code
# says whether it worked.
function Invoke-Native {
    param([scriptblock] $Command, [string] $What)
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & $Command
    } finally {
        $ErrorActionPreference = $previous
    }
    if ($LASTEXITCODE -ne 0) { throw "$What failed ($LASTEXITCODE)" }
}

$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
# Everything meant to be shipped lands here: the zip, the installer, and the
# RTF licence the installer UI needs on the way. Named for what it holds
# rather than for the tool that fills it, since the zip predates WiX running
# and does not need it at all.
$outputDir = Join-Path $root 'target\dist'
$binary = Join-Path $root 'target\release\aitch.exe'

if (-not $Version) {
    $cargo = Get-Content (Join-Path $root 'Cargo.toml') -Raw
    if ($cargo -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
        throw 'Could not read the version out of Cargo.toml'
    }
    $Version = $Matches[1]
}
Write-Host "Packaging Aitch $Version"

if (-not $SkipBuild) {
    # Rust puts the absolute path of every source file into panic messages and
    # debug info, which means the build machine's home directory and its
    # CARGO_HOME end up in the shipped binary. `trim-paths` would do this in
    # the profile, but it is not stable in cargo 1.98, so remap by hand.
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
    $flags = @(
        "--remap-path-prefix=$cargoHome=[cargo]",
        "--remap-path-prefix=$root=[aitch]"
    )
    if ($env:USERPROFILE) {
        $flags += "--remap-path-prefix=$env:USERPROFILE=[home]"
    }

    # CARGO_ENCODED_RUSTFLAGS, not RUSTFLAGS: the latter is split on spaces,
    # so a checkout under a path like "Code Projects" would tear a flag in
    # half. This one is separated by U+001F and carries spaces intact.
    $encoded = $flags -join [char]0x1f

    Push-Location $root
    $previous = $env:CARGO_ENCODED_RUSTFLAGS
    try {
        $env:CARGO_ENCODED_RUSTFLAGS = $encoded
        # cargo writes its progress to stderr, and with ErrorActionPreference
        # set to Stop that is a terminating error the moment this script's
        # output is piped anywhere. The exit code is the thing to believe.
        Invoke-Native { cargo build --release -p aitch } 'cargo build'
    } finally {
        $env:CARGO_ENCODED_RUSTFLAGS = $previous
        Pop-Location
    }
}

if (-not (Test-Path $binary)) {
    throw "No release binary at $binary. Run without -SkipBuild."
}

# Nothing about this machine goes out in a published binary. This is a hard
# failure, not a warning: it is far easier to notice here than after upload.
$bytes = [System.IO.File]::ReadAllBytes($binary)
$text = [System.Text.Encoding]::ASCII.GetString($bytes)
$leaks = [regex]::Matches($text, '[A-Za-z]:\\Users\\[A-Za-z0-9_.-]+') |
    ForEach-Object { $_.Value } | Sort-Object -Unique
if ($leaks) {
    throw ("The release binary carries paths from the machine that built it: " +
           ($leaks -join ', ') +
           ". Rebuild without -SkipBuild so the remapping is applied.")
}
Write-Host 'Binary carries no build-machine paths'

New-Item -ItemType Directory -Force -Path $outputDir | Out-Null

# The installer UI shows the licence, and insists on RTF. This is the
# smallest RTF that renders plain text correctly: escape the backslashes and
# braces, and turn line breaks into paragraph breaks. Each -replace is
# parenthesised because PowerShell will not chain them.
$licenseRtf = Join-Path $outputDir 'license.rtf'
$licenseText = Get-Content (Join-Path $root 'LICENSE') -Raw
$escaped = ($licenseText -replace '\\', '\\')
$escaped = ($escaped -replace '\{', '\{')
$escaped = ($escaped -replace '\}', '\}')
$escaped = ($escaped -replace "`r`n", "`n")
$escaped = ($escaped -replace "`n", "\par`n")
$rtf = '{\rtf1\ansi\deff0{\fonttbl{\f0\fnil\fcharset0 Segoe UI;}}' + "`n" +
       '\fs18' + "`n" + $escaped + "`n" + '}'
Set-Content -Path $licenseRtf -Value $rtf -Encoding ascii

# The docs that ship beside the binary, as they are named in aitch.wxs: source
# path relative to the repository root, then the name it lands under. Both the
# installer and the portable zip flatten them next to the executable, so the
# two carry the same files under the same names. A missing one is a build
# failure rather than a silent omission;
# crates/aitch-harness/tests/documentation.rs checks the same list.
$docs = [ordered] @{
    'README.md'      = 'README.md'
    'LICENSE'        = 'LICENSE'
    'docs\guide.md'  = 'guide.md'
    'docs\config.md' = 'config.md'
    'docs\keymap.md' = 'keymap.md'
}
foreach ($doc in $docs.Keys) {
    if (-not (Test-Path (Join-Path $root $doc))) {
        throw "aitch.wxs ships $doc, which is not there"
    }
}

# The portable zip: the same binary and the same documents, with nothing to
# install and nothing written outside the folder it is unpacked into. It is
# built before the installer because it needs no toolchain beyond PowerShell,
# so a machine without WiX can still produce something people can run.
$stageName = "aitch-$Version-x86_64-windows"
$stage = Join-Path $outputDir $stageName
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Copy-Item $binary (Join-Path $stage 'aitch.exe')
foreach ($doc in $docs.Keys) {
    Copy-Item (Join-Path $root $doc) (Join-Path $stage $docs[$doc])
}

# Everything sits under one folder inside the archive, so unpacking it in a
# downloads directory does not scatter six files across it.
$zip = Join-Path $outputDir "$stageName.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $stage -DestinationPath $zip -CompressionLevel Optimal
Remove-Item -Recurse -Force $stage
$zipSize = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host "Built $zip ($zipSize MB)"

# The installer UI comes from an extension package, which is not part of the
# wix tool itself. Adding one that is already there is a no-op, and this beats
# error WIX0144 four minutes into a build.
Invoke-Native { wix extension add --global WixToolset.UI.wixext/5.0.2 } 'wix extension add'

$msi = Join-Path $outputDir "aitch-$Version-x86_64.msi"

Invoke-Native {
    wix build (Join-Path $PSScriptRoot 'aitch.wxs') `
        -define "Version=$Version" `
        -define "BinaryPath=$binary" `
        -define "DocsPath=$root" `
        -define "LicenseRtf=$licenseRtf" `
        -ext WixToolset.UI.wixext `
        -arch x64 `
        -out $msi
} 'wix build' 

$size = [math]::Round((Get-Item $msi).Length / 1MB, 1)
Write-Host "Built $msi ($size MB)"
