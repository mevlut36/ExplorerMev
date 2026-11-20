<#
  Dessine l'icône d'ExplorerMev (dessin original, aucune ressource tierce) et produit :
    - crates/fx-app/res/app.ico            (16, 24, 32, 48, 64, 128, 256 px, PNG dans l'ICO)
    - packaging/msix/Assets/*.png          (images exigées par le manifeste MSIX)
  À relancer seulement pour changer le dessin ; les fichiers produits sont versionnés.
#>
param([string]$Root = (Split-Path -Parent $PSScriptRoot))

Add-Type -AssemblyName System.Drawing

function New-RoundedRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

# Dessin carré de côté $size : fond bleu arrondi, dossier jaune au premier plan.
function Draw-Icon([int]$size, [int]$width = 0, [int]$height = 0) {
    if ($width -eq 0) { $width = $size; $height = $size }
    $bmp = New-Object System.Drawing.Bitmap $width, $height, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)
    # Carré centré dans la zone (icônes larges : marges de chaque côté).
    $s = [Math]::Min($width, $height)
    $ox = ($width - $s) / 2.0
    $oy = ($height - $s) / 2.0
    $m = $s * 0.04
    $bg = New-RoundedRect ($ox + $m) ($oy + $m) ($s - 2 * $m) ($s - 2 * $m) ($s * 0.20)
    $p1 = [System.Drawing.PointF]::new($ox, $oy)
    $p2 = [System.Drawing.PointF]::new($ox + $s, $oy + $s)
    $c1 = [System.Drawing.Color]::FromArgb(255, 0x1F, 0x6F, 0xEB)
    $c2 = [System.Drawing.Color]::FromArgb(255, 0x0B, 0x2F, 0x7A)
    $grad = [System.Drawing.Drawing2D.LinearGradientBrush]::new($p1, $p2, $c1, $c2)
    $g.FillPath($grad, $bg)
    # Dossier : onglet puis corps.
    $fx = $ox + $s * 0.18; $fw = $s * 0.64
    $fy = $oy + $s * 0.30; $fh = $s * 0.46
    $tab = New-RoundedRect $fx ($fy - $s * 0.07) ($fw * 0.42) ($s * 0.14) ($s * 0.04)
    $back = [System.Drawing.Color]::FromArgb(255, 0xE8, 0xB8, 0x4A)
    $front = [System.Drawing.Color]::FromArgb(255, 0xFF, 0xD9, 0x6B)
    $g.FillPath((New-Object System.Drawing.SolidBrush $back), $tab)
    $body = New-RoundedRect $fx $fy $fw $fh ($s * 0.06)
    $g.FillPath((New-Object System.Drawing.SolidBrush $back), $body)
    $flap = New-RoundedRect $fx ($fy + $fh * 0.22) $fw ($fh * 0.78) ($s * 0.06)
    $g.FillPath((New-Object System.Drawing.SolidBrush $front), $flap)
    # Trait de recherche (loupe stylisée) : rappelle la recherche instantanée.
    if ($s -ge 48) {
        $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 0x0B, 0x2F, 0x7A)), ($s * 0.045)
        $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
        $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
        $cx = $ox + $s * 0.56; $cy = $oy + $s * 0.56; $r = $s * 0.10
        $g.DrawEllipse($pen, $cx - $r, $cy - $r, 2 * $r, 2 * $r)
        $g.DrawLine($pen, $cx + $r * 0.72, $cy + $r * 0.72, $cx + $r * 1.7, $cy + $r * 1.7)
    }
    $g.Dispose()
    return $bmp
}

function Save-Png($bmp, [string]$path) {
    $dir = Split-Path -Parent $path
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force $dir | Out-Null }
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
}

function Png-Bytes($bmp) {
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    return , $ms.ToArray()
}

# --- app.ico : entrées PNG (Windows Vista et suivants) ---
$sizes = 16, 24, 32, 48, 64, 128, 256
$images = foreach ($s in $sizes) { $b = Draw-Icon $s; , (Png-Bytes $b); $b.Dispose() }
$ms = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter $ms
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]
    $w.Write([byte]($(if ($s -ge 256) { 0 } else { $s })))
    $w.Write([byte]($(if ($s -ge 256) { 0 } else { $s })))
    $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$images[$i].Length); $w.Write([uint32]$offset)
    $offset += $images[$i].Length
}
foreach ($img in $images) { $w.Write($img) }
$w.Flush()
$icoPath = Join-Path $Root "crates\fx-app\res\app.ico"
[System.IO.File]::WriteAllBytes($icoPath, $ms.ToArray())
"app.ico : {0} octets, {1} tailles" -f (Get-Item $icoPath).Length, $sizes.Count

# --- images MSIX ---
$assets = Join-Path $Root "packaging\msix\Assets"
$specs = @(
    @{ Name = "Square44x44Logo.png"; W = 44; H = 44 },
    @{ Name = "Square44x44Logo.targetsize-24_altform-unplated.png"; W = 24; H = 24 },
    @{ Name = "Square44x44Logo.targetsize-256_altform-unplated.png"; W = 256; H = 256 },
    @{ Name = "Square150x150Logo.png"; W = 150; H = 150 },
    @{ Name = "Wide310x150Logo.png"; W = 310; H = 150 },
    @{ Name = "StoreLogo.png"; W = 50; H = 50 }
)
foreach ($sp in $specs) {
    $b = Draw-Icon ([Math]::Min($sp.W, $sp.H)) $sp.W $sp.H
    Save-Png $b (Join-Path $assets $sp.Name)
    $b.Dispose()
}
"images MSIX : " + $specs.Count + " dans " + $assets
