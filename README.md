# WinDir

Explorateur de fichiers pour Windows 11, écrit en Rust, conçu pour être nettement plus rapide et
plus léger que l'Explorateur Windows.

> **État : phase 4 terminée (fonctions avancées).** Navigation, icônes, sélection multiple,
> renommage, corbeille, presse-papiers, menu contextuel natif, glisser-déposer,
> rafraîchissement automatique, partages réseau annulables, état OneDrive ; onglets avec
> restauration de session, double volet, vue icônes avec vignettes, volet d'aperçu (texte,
> images, PDF/Office par les prévisualiseurs de Windows), palette de commandes, taille des
> dossiers, favoris, terminal, copie du chemin, propriétés, option « Remplacer Win+E ».
> Recherche instantanée : phase 5.

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
| Ctrl+T, Ctrl+W, Ctrl+Maj+T | Nouvel onglet, fermer l'onglet, rouvrir le dernier fermé |
| Ctrl+Tab, Ctrl+Maj+Tab | Onglet suivant, précédent |
| F6 | Double volet (Tab passe d'un volet à l'autre) |
| F5 / Maj+F5 (double volet) | Copier / déplacer la sélection vers l'autre volet (Ctrl+R actualise) |
| Ctrl+Maj+V | Vue détails ou vue icônes (flèches : déplacement en grille) |
| Alt+P | Volet d'aperçu |
| Ctrl+Maj+P | Palette de commandes (toutes les commandes, avec recherche) |
| Ctrl+Maj+S | Calculer la taille des dossiers sélectionnés (en arrière-plan) |
| Ctrl+D | Ajouter aux favoris (volet de navigation) |
| Ctrl+Maj+C | Copier le chemin d'accès |
| Ctrl+Maj+` | Ouvrir le dossier dans le terminal |
| Alt+Entrée | Propriétés (boîte de dialogue de Windows) |

## Configuration

`%APPDATA%\WinDir\config.toml`, créé avec ses commentaires au premier lancement : moteur de
rendu, Mica, dossier de départ, éléments masqués, délai réseau (10 s par défaut), niveau du
journal, affichage des nouveaux volets (`view = "details"` ou `"icons"`). Une valeur invalide est ignorée avec un avertissement ; un fichier illisible n'est
jamais écrasé. Les options de la ligne de commande sont prioritaires (`windir --help`).

Les onglets (et leur double volet) sont restaurés au lancement sans dossier en argument
(`%APPDATA%\WinDir\session.txt`) ; les favoris sont dans `%APPDATA%\WinDir\favoris.txt`.

**Remplacer Win+E** (désactivé par défaut) : commande « Remplacer Win+E par WinDir… » de la
palette (confirmée), ou `windir --win-e on`. « Rendre Win+E à l'Explorateur » ou
`windir --win-e off` rétablit le comportement d'origine ; `windir --win-e status` affiche l'état.
Seule une clé de l'utilisateur courant est écrite ; WinDir refuse si un autre programme a déjà
pris Win+E.

La colonne « État » montre l'état OneDrive : nuage (en ligne seulement), coche (disponible
localement), coche pleine (toujours conservé).

## Mesures (machine de développement)

| Cible | Résultat |
|---|---|
| Démarrage < 250 ms | 74–83 ms (à froid approché : 78–79 ms) |
| Mémoire au repos < 60 Mo | 16,7 Mo privés (dossier de 1 000 éléments ; 22,8 Mo en vue icônes) |
| 100 000 fichiers, premier affichage < 150 ms | 9–16 ms |
| 100 000 fichiers, liste triée < 1 s | 61–72 ms |
| Défilement à 60 i/s, 100 000 éléments | vue détails : 0 image > 16,7 ms (fenêtre et plein écran) ; vue icônes : idem, sauf sauts aléatoires en plein écran (81 i/s) |
| Thread d'interface jamais bloqué | latence max 12 ms, 0 mesure > 50 ms |

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
