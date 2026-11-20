# WinDir

Explorateur de fichiers pour Windows 11, écrit en Rust, conçu pour être nettement plus rapide et
plus léger que l'Explorateur Windows.

> **État : phase 3 terminée (robustesse).** Navigation, volet et fil d'Ariane, icônes,
> sélection multiple, renommage, corbeille, presse-papiers, menu contextuel natif,
> glisser-déposer, rafraîchissement automatique ; partages réseau avec délai et annulation,
> état OneDrive, éléments masqués, configuration. Toutes les cibles de performance mesurées
> (hors recherche, phase 5).

## Compilation et lancement

Prérequis :

- Windows 11 x64
- Rust stable (1.85 ou plus, édition 2024) : `winget install Rustlang.Rustup`
- Visual Studio 2022 Build Tools : MSVC v143 et SDK Windows (testé avec MSVC 14.44 et
  SDK 10.0.26100)

```powershell
cargo build --release
target\release\windir.exe                       # profil utilisateur
target\release\windir.exe "D:\Projets"          # dossier de départ
target\release\windir.exe --help
```

## Raccourcis

| Touche | Action |
|---|---|
| Entrée / double-clic | Ouvrir |
| Retour arrière, Alt+Haut | Dossier parent (resélectionne le dossier d'origine) |
| Alt+Gauche / Alt+Droite | Précédent / suivant |
| F5 | Actualiser |
| F2 | Renommer |
| Suppr / Maj+Suppr | Corbeille / suppression définitive (confirmée par Windows) |
| Ctrl+C, Ctrl+X, Ctrl+V | Copier, couper, coller (compatible avec l'Explorateur) |
| Ctrl+Maj+N | Nouveau dossier |
| Ctrl+A, Ctrl+clic, Maj+clic, rectangle | Sélection |
| Lettres | Sauter à l'élément qui commence par le texte tapé |
| Maj+F10, touche Menu, clic droit | Menu contextuel de Windows |
| Ctrl+L, Alt+D, F4 | Barre d'adresse (Tab complète, flèches parcourent les suggestions) |
| Ctrl+F, Ctrl+E | Filtre du dossier courant (Échap pour l'effacer) |
| Ctrl+H | Afficher ou masquer les éléments cachés |
| Échap (pendant un chargement) | Abandonner le chargement (partage réseau qui ne répond pas) |
| Clic sur un en-tête | Tri (un second clic inverse le sens) |

## Configuration

`%APPDATA%\WinDir\config.toml`, créé avec ses commentaires au premier lancement : moteur de
rendu, Mica, dossier de départ, éléments masqués, délai réseau (10 s par défaut), niveau du
journal. Une valeur invalide est ignorée avec un avertissement ; un fichier illisible n'est
jamais écrasé. Les options de la ligne de commande sont prioritaires (`windir --help`).

La colonne « État » montre l'état OneDrive : nuage (en ligne seulement), coche (disponible
localement), coche pleine (toujours conservé).

## Mesures (machine de développement)

| Cible | Résultat |
|---|---|
| Démarrage < 250 ms | 63–121 ms (à froid approché : 70–77 ms) |
| Mémoire au repos < 60 Mo | 15,8 Mo privés (dossier de 1 000 éléments) |
| 100 000 fichiers, premier affichage < 150 ms | 9–16 ms |
| 100 000 fichiers, liste triée < 1 s | 59–67 ms |
| Défilement à 60 i/s, 100 000 éléments | 0 image > 16,7 ms (fenêtre et plein écran) |
| Thread d'interface jamais bloqué | latence max 18 ms, 0 mesure > 50 ms |

Détails et limites : [docs/mesures/phase3.md](docs/mesures/phase3.md).

## Organisation

| Crate | Rôle |
|---|---|
| `crates/fx-win` | Seul crate avec `unsafe` : listing, chemins `\\?\`, DWM, Shell, locale |
| `crates/fx-core` | Modèle sans interface : dossier compact, tri naturel, filtre, historique, chargeur |
| `crates/fx-ui` | Interface Slint (style fluent, moteur de rendu logiciel) |
| `crates/fx-app` | Binaire `windir.exe` : manifeste, journalisation, options |
| `benches` | Benchmarks criterion et générateur de jeux de fichiers |
| `prototypes/slint-scroll` | Prototype jetable de la phase 0 |

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- Décisions : [0001 Interface](docs/adr/0001-framework-ui.md) ·
  [0002 Listing](docs/adr/0002-methode-listing.md) ·
  [0003 Index](docs/adr/0003-strategie-index.md) ·
  [0004 Panics](docs/adr/0004-strategie-panic.md)
- Mesures : [phase 0](docs/mesures/phase0-prototype.md) · [phase 1](docs/mesures/phase1.md) · [phase 2](docs/mesures/phase2.md) · [phase 3](docs/mesures/phase3.md)
- [Notes Win32](docs/NOTES-WIN32.md)

## Développement

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run --release -p fx-bench --bin gen-fixtures   # une fois
cargo bench -p fx-bench
target\release\windir.exe --self-test "$env:TEMP\windir-selftest"   # scénario fonctionnel, dossier vide
```

Journal : `%LOCALAPPDATA%\WinDir\logs`, niveau réglé par la variable `WINDIR_LOG`
(`error`, `warn`, `info`, `debug`, `trace`, `off`).
