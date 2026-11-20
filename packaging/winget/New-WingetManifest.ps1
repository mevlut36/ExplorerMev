<#
  Génère les manifestes winget (schéma 1.6.0) de l'archive portable.

  L'archive portable est le format retenu pour winget : elle s'installe sans certificat. Le
  paquet MSIX, signé par un certificat auto-signé, ne s'installerait pas chez un utilisateur ;
  une entrée MSIX pourra être ajoutée à Installers quand un certificat de signature de code
  approuvé sera disponible.

  Les champs Publisher, License et PublisherUrl sont à compléter avant toute soumission au
  dépôt winget-pkgs (voir README.md, section Distribution).
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][string]$InstallerUrl,
    [Parameter(Mandatory)][string]$Zip,
    [Parameter(Mandatory)][string]$OutDir,
    [string]$Identifier = "WinDir.WinDir",
    [string]$Publisher = "WinDir",
    [string]$License = "Proprietary"
)
$ErrorActionPreference = "Stop"
# Attention : PowerShell ignore la casse des noms de variables ($version écraserait $Version).
if (-not (Test-Path $Zip)) { throw "archive introuvable : $Zip" }
$sha = (Get-FileHash $Zip -Algorithm SHA256).Hash.ToUpper()
New-Item -ItemType Directory -Force $OutDir | Out-Null
$schema = "1.6.0"
$enc = New-Object System.Text.UTF8Encoding $false

$yamlVersion = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.version.$schema.schema.json
PackageIdentifier: $Identifier
PackageVersion: $Version
DefaultLocale: fr-FR
ManifestType: version
ManifestVersion: $schema
"@

$yamlInstaller = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.installer.$schema.schema.json
PackageIdentifier: $Identifier
PackageVersion: $Version
MinimumOSVersion: 10.0.22000.0
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
- RelativeFilePath: windir.exe
  PortableCommandAlias: windir
Installers:
- Architecture: x64
  InstallerUrl: $InstallerUrl
  InstallerSha256: $sha
ManifestType: installer
ManifestVersion: $schema
"@

$yamlLocale = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.defaultLocale.$schema.schema.json
PackageIdentifier: $Identifier
PackageVersion: $Version
PackageLocale: fr-FR
Publisher: $Publisher
PackageName: WinDir
License: $License
ShortDescription: Explorateur de fichiers rapide et léger pour Windows 11, avec recherche instantanée.
Description: >-
  WinDir est un explorateur de fichiers écrit en Rust : démarrage en moins de 100 ms, onglets,
  double volet, aperçu, vue icônes avec vignettes, palette de commandes et recherche par nom sur
  tous les volumes NTFS.
Tags:
- explorateur
- fichiers
- file-manager
- recherche
ManifestType: defaultLocale
ManifestVersion: $schema
"@

[System.IO.File]::WriteAllText((Join-Path $OutDir "$Identifier.yaml"), $yamlVersion, $enc)
[System.IO.File]::WriteAllText((Join-Path $OutDir "$Identifier.installer.yaml"), $yamlInstaller, $enc)
[System.IO.File]::WriteAllText((Join-Path $OutDir "$Identifier.locale.fr-FR.yaml"), $yamlLocale, $enc)
"Manifestes winget : $OutDir (SHA256 $sha)"
