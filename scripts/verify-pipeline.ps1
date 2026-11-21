# verify-pipeline.ps1 — end-to-end check of the engine on Windows.
#
# 1. builds a Minecraft-shaped jar with PowerShell's own ZIP writer
#    (Compress-Archive → real DEFLATE, not our code),
# 2. extracts it with our inflate path and compares SHA-256 per file,
# 3. ports it to a .hzp and prints the report,
# 4. runs the engine self-test.
#
# Run:  pwsh -File scripts\verify-pipeline.ps1
$ErrorActionPreference = 'Continue'  # native stderr must not abort the script
$failed = $false
$root = Split-Path -Parent $PSScriptRoot
$engine = Join-Path $root 'engine'
$work = Join-Path $env:TEMP 'hz-verify'
$hz = Join-Path $engine 'target\release\hz.exe'

if (-not (Test-Path $hz)) { throw "build first: cargo build --release (missing $hz)" }

Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
$src = Join-Path $work 'jar'
New-Item -ItemType Directory -Force $src, (Join-Path $work 'out') | Out-Null

# --- a small but representative jar -----------------------------------------
$blockstates = @{ variants = @{
    '' = @{ model = 'minecraft:block/cube_all' }
    'facing=north' = @{ model = 'minecraft:block/cube_all'; x = 90 }
    'facing=south' = @{ model = 'minecraft:block/cube_all'; x = 270; uvlock = $true }
} }
$model = @{
    textures = @{ all = 'minecraft:block/stone'; particle = '#all' }
    elements = @(
        @{ from = @(0, 0, 0); to = @(16, 16, 16); faces = @{
            up    = @{ texture = '#all'; cullface = 'up' }
            down  = @{ texture = '#all'; cullface = 'down' }
            north = @{ texture = '#all'; cullface = 'north' }
            south = @{ texture = '#all'; cullface = 'south' }
            west  = @{ texture = '#all'; cullface = 'west' }
            east  = @{ texture = '#all'; cullface = 'east' }
        }; rotation = @{ origin = @(8, 8, 8); axis = 'y'; angle = 22.5 } }
    )
}
$lang = @{ 'block.minecraft.stone' = 'Stone'; 'item.minecraft.apple' = 'Apple' }
$recipe = @{ type = 'minecraft:crafting_shaped'; pattern = @('X'); key = @{ X = @{ item = 'minecraft:stone' } } }

$files = @{
    'assets/minecraft/blockstates/stone.json' = ($blockstates | ConvertTo-Json -Depth 8)
    'assets/minecraft/blockstates/air.json'   = '{"variants":{"":{"model":"minecraft:block/cube_all"}}}'
    'assets/minecraft/models/block/cube_all.json' = ($model | ConvertTo-Json -Depth 12)
    'assets/minecraft/lang/en_us.json'        = ($lang | ConvertTo-Json -Depth 4)
    'assets/minecraft/lang/de_de.json'        = '{"block.minecraft.stone":"Stein"}'
    'data/minecraft/recipe/stone.json'        = ($recipe | ConvertTo-Json -Depth 6)
    'data/minecraft/loot_table/blocks/stone.json' = '{"type":"minecraft:block","pools":[]}'
    'net/minecraft/world/level/Level.class'   = 'CAFEBABE'
    'example.mixins.json'                     = '{"required":true,"package":"com.example","mixins":["A"]}'
    'pack.mcmeta'                             = '{"pack":{"pack_format":75}}'
}
# a 256 KiB pseudo-random blob under data/ so inflate is exercised on
# incompressible input as well as on JSON
$rand = New-Object byte[] 262144
(New-Object System.Random 42).NextBytes($rand)
$files['data/minecraft/hz_blob.bin'] = [Convert]::ToBase64String($rand)

foreach ($k in $files.Keys) {
    $p = Join-Path $src $k
    New-Item -ItemType Directory -Force (Split-Path $p) | Out-Null
    if ($k -like '*.bin') {
        [IO.File]::WriteAllBytes($p, $rand)
    } else {
        Set-Content -Path $p -Value $files[$k] -NoNewline -Encoding utf8
    }
}

$jar = Join-Path $work 'fixture.jar'
# Compress-Archive only accepts .zip, so build the archive as .zip then rename.
$zip = Join-Path $work 'fixture.zip'
Compress-Archive -Path (Join-Path $src '*') -DestinationPath $zip -CompressionLevel Optimal
Move-Item -Force $zip $jar
Write-Host "jar: $jar ($((Get-Item $jar).Length) bytes)"

# --- 1. inflate check --------------------------------------------------------
& $hz unzip $jar (Join-Path $work 'unzipped') 2>&1 | Out-Host
$bad = 0
# `$env:TEMP` can be an 8.3 short path on a runner while Get-ChildItem returns the
# long form, so any prefix arithmetic on the two disagrees. Entering the directory
# first makes both sides use the same base.
Push-Location $src
try {
    $entries = Get-ChildItem -Recurse -File | ForEach-Object {
        $_.FullName.Substring((Get-Location).Path.Length).TrimStart('\', '/')
    }
} finally {
    Pop-Location
}
foreach ($rel in $entries) {
    $got = Join-Path $work "unzipped\$rel"
    if (-not (Test-Path $got)) { Write-Warning "missing $rel"; $bad++; continue }
    $a = (Get-FileHash (Join-Path $src $rel) -Algorithm SHA256).Hash
    $b = (Get-FileHash $got -Algorithm SHA256).Hash
    if ($a -ne $b) { Write-Warning "hash mismatch $rel"; $bad++ }
}
Write-Host "inflate check: $(if ($bad -eq 0) { 'OK' } else { "$bad mismatches" })"

# --- 2. port -----------------------------------------------------------------
& $hz port $jar (Join-Path $work 'out\fixture.hzp') --version 26.3 --quiet --report (Join-Path $work 'out\report.json') 2>&1 | Out-Host
if ($LASTEXITCODE -ne 0) { Write-Warning 'port failed'; $failed = $true }
& $hz info (Join-Path $work 'out\fixture.hzp') 2>&1 | Out-Host
Get-Content (Join-Path $work 'out\report.json') | Out-Host

# --- 3. the pack must actually load --------------------------------------
# `hz info` constructs the runtime registry, so this fails on a malformed
# BS1/MD2 even when the container itself parsed fine.
$info = & $hz info (Join-Path $work 'out\fixture.hzp') 2>&1
$info | Out-Host
$infoText = ($info | Out-String)
foreach ($needle in @('blocks:', 'models:', 'quads:', 'textures:')) {
    if ($infoText -notmatch [regex]::Escape($needle)) {
        Write-Warning "hz info did not report $needle"
        $failed = $true
    }
}
if ($infoText -match 'registry: FAILED') {
    Write-Warning 'registry build failed'
    $failed = $true
}

# --- 4. frame rebuild timing ------------------------------------------------
& $hz bench (Join-Path $work 'out\fixture.hzp') 8 2>&1 | Out-Host

# --- 5. the vendored Vorbis decoder ----------------------------------------
# Decodes the checked-in fixture: container facts come from our own parser,
# sample counts from stb_vorbis, so a disagreement shows up here.
$sound = & $hz sound-check (Join-Path $root 'engine\tests\data\tiny.ogg') 2>&1
$sound | Out-Host
$soundText = ($sound | Out-String)
if ($soundText -notmatch 'decoded:') {
    Write-Warning 'sound-check did not report decoded audio'
    $failed = $true
}
if ($soundText -match 'decode failed') {
    Write-Warning 'Vorbis decode failed'
    $failed = $true
}

# --- 6. shader translation ---------------------------------------------------
# Translates the checked-in pack and asserts the output has both entry points.
# CI additionally feeds the result to `xcrun metal`; here we can only check shape.
$shaderRes = & $hz shader-translate (Join-Path $root 'engine\tests\data\shader_pack.zip') `
    (Join-Path $work 'terrain.metal') 2>&1
$shaderRes | Out-Host
if ($LASTEXITCODE -ne 0) {
    Write-Warning 'shader translation failed'
    $failed = $true
}
$mslPath = Join-Path $work 'terrain.metal'
if (Test-Path $mslPath) {
    $msl = Get-Content $mslPath -Raw
    foreach ($needle in @('vertex HzVaryings hz_vertex', 'fragment float4 hz_fragment', 'HzUniforms')) {
        if ($msl -notmatch [regex]::Escape($needle)) {
            Write-Warning "generated MSL is missing: $needle"
            $failed = $true
        }
    }
} else {
    Write-Warning 'shader-translate wrote no file'
    $failed = $true
}

# --- 6b. the translated MSL must survive a round trip through the container ---
$shaded = Join-Path $work 'shaded.jar'
$shadedPack = Join-Path $work 'shaded.hzp'
Copy-Item (Join-Path $root 'engine\tests\data\shader_pack.zip') (Join-Path $work 'pack.zip') -Force
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zipOut = [System.IO.Compression.ZipFile]::Open($shaded, 'Create')
try {
    # a minimal jar body plus the shader pack's own entries
    $entries = @{
        'assets/minecraft/blockstates/stone.json' = '{"variants":{"":{"model":"minecraft:block/stone"}}}'
        'assets/minecraft/models/block/stone.json' = '{"textures":{"all":"minecraft:block/stone"},"elements":[]}'
    }
    foreach ($key in $entries.Keys) {
        $entry = $zipOut.CreateEntry($key)
        $writer = New-Object System.IO.StreamWriter($entry.Open())
        $writer.Write($entries[$key])
        $writer.Dispose()
    }
    $inner = [System.IO.Compression.ZipFile]::OpenRead((Join-Path $work 'pack.zip'))
    try {
        foreach ($entry in $inner.Entries) {
            $target = $zipOut.CreateEntry($entry.FullName)
            $source = $entry.Open()
            $dest = $target.Open()
            $source.CopyTo($dest)
            $dest.Dispose()
            $source.Dispose()
        }
    } finally {
        $inner.Dispose()
    }
} finally {
    $zipOut.Dispose()
}

& $hz port $shaded $shadedPack --quiet --version 26.3 2>&1 | Out-Host
$infoOut = & $hz info $shadedPack 2>&1
$infoOut | Select-String -Pattern 'shader packs' | Out-Host
if (($infoOut | Out-String) -notmatch 'with translated Metal source') {
    Write-Warning 'the ported pack does not carry translated MSL'
    $failed = $true
} elseif (($infoOut | Out-String) -notmatch '\(1 with translated') {
    Write-Warning 'translated MSL did not survive the container round trip'
    $failed = $true
}

# --- 7. engine self-test -----------------------------------------------------
& $hz selftest 2>&1 | Out-Host

if ($bad -ne 0 -or $failed) { exit 1 }
exit 0