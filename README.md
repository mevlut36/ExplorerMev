# WinDir

Explorateur de fichiers pour Windows 11, écrit en Rust, conçu pour être nettement plus rapide et
plus léger que l'Explorateur Windows.

> **État : phase 0 terminée (cadrage et prototype).** Il n'existe pas encore d'application
> utilisable. Voir [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- Décisions : [0001 Framework d'interface](docs/adr/0001-framework-ui.md) ·
  [0002 Méthode de listing](docs/adr/0002-methode-listing.md) ·
  [0003 Stratégie d'index](docs/adr/0003-strategie-index.md)
- [Mesures de la phase 0](docs/mesures/phase0-prototype.md)
- [Notes Win32](docs/NOTES-WIN32.md)

## Prérequis de compilation

- Windows 11 x64
- Rust stable (1.85 ou plus, édition 2024) : `winget install Rustlang.Rustup`
- Visual Studio 2022 Build Tools : MSVC v143 et SDK Windows (testé avec MSVC 14.44 et
  SDK 10.0.26100)

## Prototype de la phase 0

```powershell
cargo run --release -p slint-scroll -- --renderer software      # interactif
cargo run --release -p slint-scroll -- --renderer software --bench
powershell -File prototypes/slint-scroll/bench.ps1 -Runs 3       # matrice complète
```
