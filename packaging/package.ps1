<#
  Produit les livrables de distribution dans dist\ :
    - ExplorerMev_<version>_x64.msix            paquet MSIX signé
    - ExplorerMev_<version>_x64_portable.zip    archive portable (explorermev.exe)
    - ExplorerMev.cer                           certificat public de signature (auto-signé)
    - SHA256SUMS.txt                       empreintes des fichiers ci-dessus
    - winget\*.yaml                        manifestes winget (si -InstallerUrl est donné)

  Signature : sans -Thumbprint, un certificat AUTO-SIGNÉ est créé (ou réutilisé) dans le magasin
  personnel de l'utilisateur courant (CurrentUser\My). Il n'est ajouté à aucun magasin de
  confiance : pour installer le MSIX sur un poste, il faut d'abord approuver ExplorerMev.cer (voir
  README.md). Pour une vraie distribution, passer -Thumbprint d'un certificat de signature de
  code approuvé et -Publisher avec son sujet exact.

  Exemples :
    .\packaging\package.ps1
    .\packaging\package.ps1 -Thumbprint 0123… -Publisher "CN=Mon Entreprise, O=…"
    .\packaging\package.ps1 -Thumbprint 0123… -TimestampUrl http://timestamp.example/tsa
    .\packaging\package.ps1 -InstallerUrl https://…/ExplorerMev_0.1.0_x64_portable.zip
#>
[CmdletBinding()]
param(
    [string]$Thumbprint,
    [string]$Publisher = "CN=ExplorerMev (certificat de developpement auto-signe)",
    [string]$PublisherDisplay = "ExplorerMev",
    [string]$InstallerUrl,
    [string]$TimestampUrl,
    [switch]$SkipBuild
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root "dist"
$stage = Join-Path $dist "stage"

# --- outils du SDK Windows ---
$sdk = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Directory -ErrorAction SilentlyContinue |
    Where-Object { Test-Path (Join-Path $_.FullName "x64\makeappx.exe") } |
    Sort-Object Name -Descending | Select-Object -First 1
if (-not $sdk) { throw "Windows SDK introuvable (makeappx.exe, signtool.exe)." }
$makeappx = Join-Path $sdk.FullName "x64\makeappx.exe"
$signtool = Join-Path $sdk.FullName "x64\signtool.exe"

# --- version : Cargo.toml (workspace.package) ---
$cargo = Get-Content (Join-Path $root "Cargo.toml") -Raw
if ($cargo -notmatch '(?ms)\[workspace\.package\].*?^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"') { throw "version introuvable dans Cargo.toml" }
$ver3 = "$($Matches[1]).$($Matches[2]).$($Matches[3])"
$ver4 = "$ver3.0"
"Version : $ver3"

# --- compilation ---
if (-not $SkipBuild) {
    Push-Location $root
    try { cargo build --release -p fx-app; if ($LASTEXITCODE -ne 0) { throw "cargo build a échoué" } } finally { Pop-Location }
}
$exe = Join-Path $root "target\release\explorermev.exe"
if (-not (Test-Path $exe)) { throw "explorermev.exe introuvable : $exe" }

# --- certificat de signature ---
if ($Thumbprint) {
    $cert = Get-Item "Cert:\CurrentUser\My\$Thumbprint" -ErrorAction SilentlyContinue
    if (-not $cert) { $cert = Get-Item "Cert:\LocalMachine\My\$Thumbprint" -ErrorAction Stop }
    $Publisher = $cert.Subject
} else {
    $cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert | Where-Object { $_.Subject -eq $Publisher -and $_.NotAfter -gt (Get-Date).AddDays(30) } | Select-Object -First 1
    if (-not $cert) {
        "Création d'un certificat auto-signé : $Publisher"
        $cert = New-SelfSignedCertificate -Type Custom -Subject $Publisher -KeyUsage DigitalSignature `
            -FriendlyName "ExplorerMev (developpement)" -CertStoreLocation "Cert:\CurrentUser\My" -KeyExportPolicy NonExportable `
            -NotAfter (Get-Date).AddYears(2) `
            -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
    }
}
"Certificat : $($cert.Subject) ($($cert.Thumbprint))"

# --- répertoire de paquet ---
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage -Confirm:$false }
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item $exe (Join-Path $stage "explorermev.exe")
Copy-Item (Join-Path $root "packaging\msix\Assets") (Join-Path $stage "Assets") -Recurse
$xml = Get-Content (Join-Path $root "packaging\msix\AppxManifest.xml.in") -Raw -Encoding UTF8
$xml = $xml.Replace("{{VERSION}}", $ver4).Replace("{{PUBLISHER}}", [Security.SecurityElement]::Escape($Publisher)).Replace("{{PUBLISHER_DISPLAY}}", [Security.SecurityElement]::Escape($PublisherDisplay))
[System.IO.File]::WriteAllText((Join-Path $stage "AppxManifest.xml"), $xml, (New-Object System.Text.UTF8Encoding $false))

# --- MSIX ---
$msix = Join-Path $dist "ExplorerMev_${ver3}_x64.msix"
if (Test-Path $msix) { Remove-Item $msix -Force }
& $makeappx pack /d $stage /p $msix /o | Out-Host
if ($LASTEXITCODE -ne 0) { throw "makeappx a échoué" }
$signArgs = @("sign", "/fd", "SHA256", "/sha1", $cert.Thumbprint)
# Horodatage : seulement à la demande (-TimestampUrl), pour une signature avec un vrai certificat.
if ($TimestampUrl) { $signArgs += @("/tr", $TimestampUrl, "/td", "SHA256") }
& $signtool @signArgs $msix | Out-Host
if ($LASTEXITCODE -ne 0) { throw "signtool a échoué" }
Export-Certificate -Cert $cert -FilePath (Join-Path $dist "ExplorerMev.cer") | Out-Null

# --- archive portable ---
$zip = Join-Path $dist "ExplorerMev_${ver3}_x64_portable.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $exe -DestinationPath $zip -CompressionLevel Optimal

# --- empreintes ---
$sums = foreach ($f in @($msix, $zip, (Join-Path $dist "ExplorerMev.cer"))) { "{0}  {1}" -f (Get-FileHash $f -Algorithm SHA256).Hash.ToLower(), (Split-Path -Leaf $f) }
Set-Content (Join-Path $dist "SHA256SUMS.txt") $sums -Encoding ascii
$sums

# --- manifestes winget ---
if ($InstallerUrl) {
    & (Join-Path $PSScriptRoot "winget\New-WingetManifest.ps1") -Version $ver3 -InstallerUrl $InstallerUrl -Zip $zip -OutDir (Join-Path $dist "winget")
}
Remove-Item -Recurse -Force $stage -Confirm:$false
"Terminé : $dist"
