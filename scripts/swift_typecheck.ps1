#!/usr/bin/env pwsh
# swift_typecheck.ps1 — compile-level checks for the Swift sources on a host
# that has a Swift toolchain but no Xcode.
#
#   pwsh -File scripts/swift_typecheck.ps1
#
# Three levels, in increasing strength:
#
#   1. `swiftc -parse` over every .swift file — syntax, literals, attributes.
#   2. `swiftc -typecheck` over the files whose only framework dependency is
#      Foundation, importing the real C header through the bridging header.
#      HZEngine.swift lives here: 400 lines of FFI, the place a wrong pointer
#      type would actually hurt.
#   3. `swiftc -typecheck` over the model layer against a Combine shim
#      (scripts/swift_shims/Combine.swift), which is as far as a non-Apple host
#      can go: ObservableObject and @Published exist, their behaviour does not.
#
# The SwiftUI/Metal files cannot be type checked without an Apple SDK; the macOS
# CI job builds them for real with xcodebuild. This script is the fast, portable
# half of that check.
#
# Exit code: 0 clean, 1 problems, 2 no toolchain.
$ErrorActionPreference = 'Continue'

$root = Split-Path -Parent $PSScriptRoot
$app = Join-Path $root 'app'
$engineInclude = Join-Path $root 'engine\include'
$bridging = Join-Path $app 'HorizonPad\Bridge\HorizonPad-Bridging-Header.h'
$shimSource = Join-Path $PSScriptRoot 'swift_shims\Combine.swift'
$buildDir = Join-Path $root '.build\swiftshims'

foreach ($required in @($app, $engineInclude, $bridging, $shimSource)) {
    if (-not (Test-Path $required)) { Write-Error "missing path: $required"; exit 1 }
}

function Find-Swiftc {
    $cmd = Get-Command swiftc -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $candidates = @(
        (Join-Path $env:LOCALAPPDATA 'Programs\Swift\Toolchains'),
        'C:\Program Files\Swift\Toolchains',
        '/usr/bin',
        '/usr/local/bin',
        '/opt/swift/usr/bin'
    )
    foreach ($dir in $candidates) {
        if (-not (Test-Path $dir)) { continue }
        $found = Get-ChildItem $dir -Recurse -Filter swiftc* -File -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($found) { return $found.FullName }
    }
    return $null
}

function Find-Sdk([string] $swiftcPath) {
    # Windows toolchains keep the SDK in a sibling "Platforms" directory; walk up
    # from the compiler until it appears. Linux/macOS hosts need no -sdk.
    $dir = Split-Path -Parent $swiftcPath
    for ($i = 0; $i -lt 6 -and $dir; $i++) {
        $platforms = Join-Path $dir 'Platforms'
        if (Test-Path $platforms) {
            $sdk = Get-ChildItem $platforms -Recurse -Directory -Filter 'Windows.sdk' -ErrorAction SilentlyContinue |
                Select-Object -First 1
            if ($sdk) { return $sdk.FullName }
        }
        $dir = Split-Path -Parent $dir
    }
    return $null
}

$swiftc = Find-Swiftc
if (-not $swiftc) {
    Write-Host 'no Swift toolchain found; skipping (winget install Swift.Toolchain)' -ForegroundColor Yellow
    exit 2
}
$sdk = Find-Sdk $swiftc
Write-Host "swiftc: $swiftc"
& $swiftc --version | Select-Object -First 1
if ($sdk) { Write-Host "sdk: $sdk" }

$sources = Get-ChildItem $app -Recurse -Filter *.swift | Sort-Object FullName
if (-not $sources) { Write-Error 'no Swift sources found'; exit 1 }
Write-Host "sources: $($sources.Count)"

$failed = $false
$baseArgs = @('-swift-version', '5')
if ($sdk) { $baseArgs += @('-sdk', $sdk) }

function Invoke-Step([string] $label, [string[]] $arguments) {
    Write-Host ''
    Write-Host "== $label ==" -ForegroundColor Cyan
    $output = & $swiftc @arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        $output | Select-Object -First 40 | Out-Host
        Write-Host "FAILED: $label" -ForegroundColor Red
        return $false
    }
    Write-Host "ok: $label" -ForegroundColor Green
    return $true
}

# --- 1. parse everything ------------------------------------------------------
if (-not (Invoke-Step "parse ($($sources.Count) files)" (@('-parse') + $baseArgs + $sources.FullName))) {
    $failed = $true
}

# --- 2. type check Foundation-only files against the real C header ------------
$foundationOnly = @(
    (Join-Path $app 'HorizonPad\Support\Paths.swift'),
    (Join-Path $app 'HorizonPad\Engine\HZEngine.swift')
)
$ffiArgs = @('-typecheck') + $baseArgs + @(
    '-import-objc-header', $bridging,
    '-Xcc', "-I$engineInclude"
) + $foundationOnly
if (-not (Invoke-Step "typecheck Foundation + hz_ffi ($($foundationOnly.Count) files)" $ffiArgs)) {
    $failed = $true
}

# --- 3. type check the model layer against the Combine shim -------------------
New-Item -ItemType Directory -Force $buildDir | Out-Null
$shimModule = Join-Path $buildDir 'Combine.swiftmodule'
$shimBuild = & $swiftc -emit-module -module-name Combine $baseArgs -emit-module-path $shimModule $shimSource 2>&1
if ($LASTEXITCODE -ne 0 -or -not (Test-Path $shimModule)) {
    Write-Host 'FAILED: build Combine shim' -ForegroundColor Red
    $shimBuild | Select-Object -First 20 | Out-Host
    $failed = $true
} else {
    $modelFiles = @(
        (Join-Path $app 'HorizonPad\Support\Paths.swift'),
        (Join-Path $app 'HorizonPad\Model\Library.swift'),
        (Join-Path $app 'HorizonPad\Model\Settings.swift')
    )
    $modelArgs = @('-typecheck') + $baseArgs + @('-I', $buildDir) + $modelFiles
    if (-not (Invoke-Step "typecheck model layer + Combine shim ($($modelFiles.Count) files)" $modelArgs)) {
        $failed = $true
    }
}

Write-Host ''
if ($failed) {
    Write-Host 'swift checks FAILED' -ForegroundColor Red
    exit 1
}
Write-Host 'swift checks passed' -ForegroundColor Green
exit 0