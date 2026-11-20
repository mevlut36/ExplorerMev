<#
.SYNOPSIS
  Reproduit les mesures de la phase 0 (prototype Slint).
.DESCRIPTION
  Compile le prototype en release, puis lance chaque configuration plusieurs fois en mode --bench.
  Les rapports bruts sont écrits dans target/bench/phase0/. Une fenêtre s'ouvre ~14 s par passage :
  ne pas toucher à la souris pendant la mesure (le survol déclenche des rendus supplémentaires).
.EXAMPLE
  pwsh prototypes/slint-scroll/bench.ps1 -Runs 3
#>
param(
    [int]$Runs = 3,
    [switch]$Skia  # ajoute la variante Skia (compile skia-bindings, ~2 min, binaires téléchargés)
)
# Pas de $ErrorActionPreference = "Stop" : sous PowerShell 5.1, la sortie stderr de cargo
# deviendrait une erreur fatale. On contrôle $LASTEXITCODE à la place.
function Assert-Ok($what) { if ($LASTEXITCODE -ne 0) { throw "$what a échoué ($LASTEXITCODE)" } }
$root = Resolve-Path "$PSScriptRoot/../.."
Push-Location $root
try {
    cargo build --release -p slint-scroll 2>&1 | Out-Null; Assert-Ok "cargo build"
    $exe = Join-Path $root "target/release/slint-scroll.exe"
    $out = Join-Path $root "target/bench/phase0"
    New-Item -ItemType Directory -Force $out | Out-Null

    $configs = @(
        @{ Name = "software-custom";           Args = @("--renderer", "software", "--view", "custom") },
        @{ Name = "software-custom-maximized"; Args = @("--renderer", "software", "--view", "custom", "--maximized") },
        @{ Name = "software-table";            Args = @("--renderer", "software", "--view", "table") },
        @{ Name = "femtovg-custom";            Args = @("--renderer", "femtovg", "--view", "custom") },
        @{ Name = "femtovg-custom-maximized";  Args = @("--renderer", "femtovg", "--view", "custom", "--maximized") },
        @{ Name = "femtovg-table";             Args = @("--renderer", "femtovg", "--view", "table") }
    )
    if ($Skia) {
        $env:CARGO_TARGET_DIR = Join-Path $root "target/skia"
        cargo build --release -p slint-scroll --features skia 2>&1 | Out-Null; Assert-Ok "cargo build skia"
        Remove-Item Env:CARGO_TARGET_DIR
        $configs += @{ Name = "skia-custom"; Args = @("--renderer", "skia"); Exe = (Join-Path $root "target/skia/release/slint-scroll.exe") }
    }

    foreach ($c in $configs) {
        $bin = if ($c.Exe) { $c.Exe } else { $exe }
        for ($i = 1; $i -le $Runs; $i++) {
            $file = Join-Path $out "$($c.Name)-run$i.txt"
            & $bin --bench @($c.Args) --out $file | Out-Null
        }
        Write-Host "=== $($c.Name) (dernier passage)"
        Get-Content (Join-Path $out "$($c.Name)-run$Runs.txt")
    }
}
finally {
    Pop-Location
}
