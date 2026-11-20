# WinDir

Explorateur de fichiers pour Windows 11, écrit en Rust, conçu pour être nettement plus rapide et
plus léger que l'Explorateur Windows.

> **État : phase 6 terminée (distribution).** Navigation, icônes, sélection multiple,
> renommage, corbeille, presse-papiers, menu contextuel natif, glisser-déposer,
> rafraîchissement automatique, partages réseau annulables, état OneDrive ; onglets avec
> restauration de session, double volet, vue icônes avec vignettes, volet d'aperçu (texte,
> images, PDF/Office par les prévisualiseurs de Windows), palette de commandes, taille des
> dossiers, favoris, terminal, copie du chemin, propriétés, option « explorateur par défaut » (Win+E et dossiers) ;
> recherche par nom sur tous les volumes NTFS (Ctrl+Maj+F), sans droits ou, sur demande, par
> la MFT. Paquet MSIX signé (certificat auto-signé), archive portable, manifestes winget.

## Installation

WinDir n'a pas encore de version publiée : les livrables se produisent avec
`packaging\package.ps1` (voir [Distribution](#distribution)).

**Archive portable** (recommandée, aucun certificat requis) : décompresser
`WinDir_<version>_x64_portable.zip` où l'on veut ; `windir.exe` est le seul fichier. Aucune
installation : WinDir n'écrit que dans `%APPDATA%\WinDir` et `%LOCALAPPDATA%\WinDir` (et, si vous
activez « explorateur par défaut », des clés de votre compte dans le registre).

**Paquet MSIX** : signé par un certificat **auto-signé** (`WinDir.cer`), que Windows ne connaît
pas. Pour l'installer sur un poste, approuver d'abord ce certificat (invite administrateur),
puis installer :

```powershell
Import-Certificate -FilePath .\WinDir.cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople
Add-AppxPackage .\WinDir_0.1.0_x64.msix
```

Ces deux commandes n'ont pas été exécutées pendant le développement (voir
[Limites connues](#limites-connues)). Pour désinstaller : `Get-AppxPackage WinDir | Remove-AppxPackage`,
puis retirer le certificat (`Get-ChildItem Cert:\LocalMachine\TrustedPeople | Where-Object Subject -like
"CN=WinDir*" | Remove-Item`). Une distribution publique exige un certificat de signature de code
approuvé.

**winget** : les manifestes de l'archive portable se génèrent avec
`packaging\winget\New-WingetManifest.ps1` ; aucun n'est publié.

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
| Ctrl+Maj+F | Rechercher sur l'ordinateur ; Entrée affiche l'élément dans son dossier, Ctrl+Entrée l'ouvre |
| Ctrl+Maj+S | Calculer la taille des dossiers sélectionnés (en arrière-plan) |
| Ctrl+D | Ajouter aux favoris (volet de navigation) |
| Ctrl+Maj+C | Copier le chemin d'accès |
| Ctrl+Maj+` | Ouvrir le dossier dans le terminal |
| Alt+Entrée | Propriétés (boîte de dialogue de Windows) |

## Configuration

`%APPDATA%\WinDir\config.toml`, créé avec ses commentaires au premier lancement : moteur de
rendu, Mica, dossier de départ, éléments masqués, délai réseau (10 s par défaut), niveau du
journal, affichage des nouveaux volets (`view = "details"` ou `"icons"`), indexation par la
MFT (`index_mft`, désactivée par défaut). Une valeur invalide est ignorée avec un avertissement ; un fichier illisible n'est
jamais écrasé. Les options de la ligne de commande sont prioritaires (`windir --help`).

Les onglets (et leur double volet) sont restaurés au lancement sans dossier en argument
(`%APPDATA%\WinDir\session.txt`) ; les favoris sont dans `%APPDATA%\WinDir\favoris.txt`.

**Explorateur par défaut** (désactivé par défaut) : commande « Utiliser WinDir comme explorateur
par défaut… » de la palette (Ctrl+Maj+P), qui demande confirmation, ou `windir --default-explorer on`.
Pour votre compte seulement, WinDir s'ouvre alors :

- avec la touche **Windows+E** ;
- quand un **dossier ou un lecteur** est ouvert depuis un autre programme, avec Win+R ou depuis le
  menu Démarrer (verbe `open` des classes `Directory` et `Drive`).

Ne change pas : `explorer.exe` reste le shell de Windows (barre des tâches, bureau, menu
Démarrer) ; remplacer le shell n'est pas pris en charge. Les fenêtres de l'Explorateur déjà
ouvertes, leur navigation interne et les programmes qui lancent `explorer.exe` directement
(par exemple « Afficher dans le dossier » de certains logiciels) ne sont pas interceptés.

Retour en arrière : « Rendre l'explorateur par défaut à Windows » (palette) ou
`windir --default-explorer off` ; `windir --default-explorer status` affiche l'état. Seules les
clés de votre compte posées par WinDir sont retirées (elles portent un marqueur) ; si un autre
programme a déjà pris un de ces emplacements, WinDir n'y touche pas. **Ne déplacez pas et ne
supprimez pas `windir.exe` tant que l'option est active** : les dossiers ouverts depuis les autres
programmes ne s'ouvriraient plus. Si cela arrive, supprimer à la main
`HKCU\Software\Classes\Directory\shell\open`, `HKCU\Software\Classes\Drive\shell\open` et
`HKCU\Software\Classes\CLSID\{52205fd8-5dfb-447d-801a-d0b52f2e83e1}`. `windir --win-e on|off`
ne règle que la touche Win+E.

**Recherche** (Ctrl+Maj+F) : index des noms de tous les volumes NTFS fixes, construit à la
première recherche en parcourant les dossiers (sans droits, en priorité arrière-plan), puis tenu
à jour par une surveillance des volumes. Il est gardé en cache dans
`%LOCALAPPDATA%\WinDir\index` (relu en 0,2 s) et libéré de la mémoire après 5 minutes sans
recherche. La commande « Indexation rapide par la MFT (administrateur)… » lance, après
confirmation et invite de Windows, un petit assistant élevé qui lit la table des fichiers et son
journal de modifications ; il ne transmet que les noms des dossiers que votre compte peut
ouvrir, et s'arrête avec WinDir (détails : [ADR 0003](docs/adr/0003-strategie-index.md)).

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
| Recherche < 50 ms par frappe | frappe → résultats : médiane 8–9 ms, max 17 ms (2,4 millions d'éléments) |
| Thread d'interface jamais bloqué | latence max 12 ms, 0 mesure > 50 ms |

Détails et limites : [phase 3](docs/mesures/phase3.md), [phase 4](docs/mesures/phase4.md),
[phase 5](docs/mesures/phase5.md).

## Limites connues

- **Indexation par la MFT** (assistant élevé) : le code, le protocole et le filtre d'accès sont
  testés, mais l'assistant n'a **jamais tourné élevé** : durée d'énumération et mémoire non
  mesurées. Le comportement de l'élévation depuis le paquet MSIX n'est pas vérifié non plus.
- **Paquet MSIX** : construit, signé et vérifié (intégrité, schéma du manifeste), mais **jamais
  installé** : le certificat est auto-signé et n'est approuvé nulle part sur la machine de
  développement. Dans le paquet, « explorateur par défaut » est refusé (le registre y est isolé) et les
  réglages sont lus et écrits dans un emplacement privé du paquet
  (`%LOCALAPPDATA%\Packages\…\LocalCache`), pas dans `%APPDATA%\WinDir`.
- **OneDrive** : jamais de lecture, de vignette ni d'aperçu d'un fichier « en ligne seulement »,
  par construction ; non vérifié sur des fichiers réels de ce type (aucun sur la machine de
  développement).
- **Aperçu** : texte, images et PDF vérifiés ; les documents Office dépendent des
  prévisualiseurs installés (non testés).
- **Recherche** : volumes NTFS fixes seulement ; un fichier à liens physiques n'est indexé que
  sous un nom ; des droits resserrés après coup ne retirent pas les noms déjà indexés avant la
  reconstruction suivante ; en mode parcours, les changements faits pendant que WinDir est
  fermé n'apparaissent qu'à la reconstruction (cache de plus de 10 minutes).
- **Vue icônes, plein écran, sauts aléatoires dans 100 000 éléments** : 81 i/s au lieu des
  60 i/s visés *constants* (quelques images lentes).
- **Mesures** : une seule machine (Ryzen 5 5600X, échelle 100 %), démarrage « à froid »
  approché (l'exécutable seul est relu depuis le disque), ouverture de dossier à froid non
  mesurée.
- **Licence** : aucune n'est définie dans le dépôt ; le manifeste winget généré porte
  `Proprietary` en attendant votre choix.

## Organisation

| Crate | Rôle |
|---|---|
| `crates/fx-win` | Seul crate avec `unsafe` : listing, chemins `\\?\`, DWM, Shell, aperçus, tube nommé, MFT/USN, contrôle d'accès, locale |
| `crates/fx-core` | Modèle sans interface : dossier compact, tri naturel, filtre, historique, chargeur, index de recherche et moteur |
| `crates/fx-ui` | Interface Slint (style fluent, moteur de rendu logiciel) |
| `crates/fx-app` | Binaire `windir.exe` : manifeste, icône, journalisation, options, assistant d'indexation élevé |
| `packaging` | Icône, manifeste MSIX, `package.ps1`, manifestes winget |
| `benches` | Benchmarks criterion et générateur de jeux de fichiers |
| `prototypes/slint-scroll` | Prototype jetable de la phase 0 |

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- Décisions : [0001 Interface](docs/adr/0001-framework-ui.md) ·
  [0002 Listing](docs/adr/0002-methode-listing.md) ·
  [0003 Index](docs/adr/0003-strategie-index.md) ·
  [0004 Panics](docs/adr/0004-strategie-panic.md)
- Mesures : [phase 0](docs/mesures/phase0-prototype.md) · [phase 1](docs/mesures/phase1.md) · [phase 2](docs/mesures/phase2.md) · [phase 3](docs/mesures/phase3.md) · [phase 4](docs/mesures/phase4.md) · [phase 5](docs/mesures/phase5.md) · [phase 6](docs/mesures/phase6.md)
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

## Distribution

```powershell
.\packaging\package.ps1                      # compile puis produit dist\ (MSIX, zip, .cer, SHA256SUMS.txt)
.\packaging\package.ps1 -SkipBuild           # réutilise target\release\windir.exe
.\packaging\package.ps1 -Thumbprint <empreinte> [-TimestampUrl <url>]   # vrai certificat de signature
.\packaging\package.ps1 -InstallerUrl <url du zip publié>               # + manifestes winget dans dist\winget
```

Prérequis : SDK Windows (`makeappx.exe`, `signtool.exe`). Sans `-Thumbprint`, le script crée un
certificat auto-signé dans le magasin **personnel** de l'utilisateur (`CurrentUser\My`) ; il ne
l'approuve nulle part. L'icône se redessine avec `packaging\make-icons.ps1`. Avant de soumettre
les manifestes à winget-pkgs : renseigner l'éditeur, la licence et les URL.

Journal : `%LOCALAPPDATA%\WinDir\logs`, niveau réglé par la variable `WINDIR_LOG`
(`error`, `warn`, `info`, `debug`, `trace`, `off`).
