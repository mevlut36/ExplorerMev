# Notes Win32 / Slint / winit

Constats sur des comportements mal documentés ou surprenants. Chaque note indique comment le
reproduire.

## Mica : `DwmExtendFrameIntoClientArea` n'est pas nécessaire avec winit

- **Constat** : `DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW)` suffit à
  afficher Mica sous les zones transparentes d'une fenêtre Slint, avec les moteurs logiciel,
  femtovg et skia. Appeler en plus `DwmExtendFrameIntoClientArea` avec des marges à -1 ne
  change rien de visible.
- **Explication probable** : winit crée la fenêtre avec `with_transparent(true)` (c'est le
  réglage par défaut de Slint sous Windows ; voir `winitwindowadapter.rs` dans
  i-slint-backend-winit 1.19) et rend ainsi la surface composée avec alpha. Le fond de fenêtre
  Slint doit valoir `transparent`.
- **Reproduction** : `PROTO_NO_EXTEND_FRAME=1 slint-scroll --renderer software`, puis comparer
  la capture avec celle obtenue sans la variable.
- **Conséquence** : `fx-win` n'appelle que `DwmSetWindowAttribute`. La barre de titre suit le
  thème via `DWMWA_USE_IMMERSIVE_DARK_MODE`.

## Moteur logiciel Slint : pas de rendering notifier

- **Constat** : `Window::set_rendering_notifier` renvoie `SetRenderingNotifierError::Unsupported`
  avec le moteur `software`.
- **Contournement** : horodater `WindowEvent::RedrawRequested` via
  `WinitWindowAccessor::on_winit_window_event`. On obtient le début de chaque image ; pour la fin
  du rendu, un `Timer::single_shot(Duration::ZERO)` armé à ce moment donne une borne haute.

## Moteur skia (Direct3D) : notifier accepté mais jamais appelé

- **Constat** : `set_rendering_notifier` réussit, mais aucun `BeforeRendering` ni
  `AfterRendering` n'est reçu pendant 12 s de défilement. Point non approfondi, le moteur étant
  écarté pour sa mémoire (226 Mo privés).

## `slint::Window::set_maximized` ignoré au démarrage

- **Constat** : `set_maximized(true)`, appelé avant ou juste après `show()`, reste sans effet
  (la fenêtre garde 1100×720). Avec winit, la fenêtre native n'est créée qu'au démarrage de la
  boucle d'événements.
- **Contournement** : appeler `winit::window::Window::set_maximized(true)` au premier événement
  de fenêtre (`on_winit_window_event`). Pour `fx-app` (restauration de session) : appliquer
  l'état de fenêtre à cet endroit.

## Pilotes GPU : coût mémoire dans le processus

- **Constat** : le pilote OpenGL AMD (Radeon RX 6600 XT, 32.0.21045.29003) ajoute environ
  160 Mo de mémoire privée et environ 230 ms au démarrage d'un processus femtovg. Skia (Direct3D)
  est encore plus lourd (226 Mo).
- **Conséquence** : ADR 0001, moteur logiciel par défaut.

## Fenêtre auxiliaire WGL

- **Constat** : avec femtovg, `Process.MainWindowHandle` (PowerShell/.NET) renvoie au début une
  fenêtre de 16×16 px créée pour charger les extensions WGL, et non la fenêtre principale.
- **Conséquence** : les outils de test qui cherchent la fenêtre doivent filtrer par titre.

## PowerShell 5.1 et sortie d'erreur des programmes natifs

- **Constat** : avec `$ErrorActionPreference = "Stop"`, la simple progression de cargo sur
  stderr (« Compiling… ») interrompt le script.
- **Conséquence** : les scripts du dépôt testent `$LASTEXITCODE` au lieu de passer par
  `ErrorActionPreference`.

---

## Phase 1

### `LCMapStringEx` + `SORT_DIGITSASNUMBERS` reproduit `StrCmpLogicalW`

- **Constat** : une clé de tri obtenue par `LCMapStringEx(LOCALE_NAME_USER_DEFAULT,
  LCMAP_SORTKEY | NORM_IGNORECASE | SORT_DIGITSASNUMBERS, …)`, comparée octet par octet, donne
  le même ordre que `StrCmpLogicalW` (la comparaison de l'Explorateur) : **0 désaccord sur
  20 000 paires** aléatoires (accents, casse, chiffres, ponctuation, émoji), ni sur une liste de
  cas choisis.
- **Intérêt** : les clés se calculent une fois par nom, en parallèle, et le tri se réduit à
  des `memcmp` (2 ms pour 100 000 noms au lieu d'environ 1,7 million d'appels à
  `StrCmpLogicalW`).
- **Reproduction** : `cargo test -p fx-win --test natural_sort -- --nocapture`.

### `LCMapStringEx(LCMAP_SORTKEY)` : taille de destination en octets

- **Constat** : avec `LCMAP_SORTKEY`, `cchDest` compte des **octets**, alors que le wrapper du
  crate `windows` (0.62) le calcule à partir d'une tranche `&mut [u16]`. On passe donc une
  tranche de N `u16` annoncée comme N octets (sûr : la capacité réelle est 2N), puis on relit
  les octets écrits. Voir `fx_win::locale::natural_sort_key`.

### `FILE_FULL_DIR_INFO.EaSize` contient le tag de point d'analyse

- **Constat** : pour une entrée `FILE_ATTRIBUTE_REPARSE_POINT`, `EaSize` de
  `FileFullDirectoryInfo` vaut le tag (`IO_REPARSE_TAG_MOUNT_POINT` pour une jonction), comme
  `dwReserved0` pour `FindFirstFileExW`.
- **Reproduction** : `cargo test -p fx-win --test listing junction`.
- **Non vérifié** : les liens symboliques. Le test correspondant est ignoré sur la machine de
  développement : la création de lien exige le mode développeur ou des droits d'administration.

### `FILE_FLAG_BACKUP_SEMANTICS` ouvre aussi les fichiers

- **Constat** : `CreateFileW(…, FILE_FLAG_BACKUP_SEMANTICS)` réussit sur un fichier ; c'est
  `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)` qui échoue ensuite, avec
  `ERROR_INVALID_PARAMETER`, la même erreur que « classe non prise en charge ». Sans précaution,
  on déclencherait le repli au lieu de signaler « ce n'est pas un dossier ».
- **Parade** : vérifier `FILE_ATTRIBUTE_DIRECTORY` via `FileBasicInfo` juste après l'ouverture.

### Racine de volume vide

- `FindFirstFileExW("X:\*")` sur une racine sans entrée renvoie `ERROR_FILE_NOT_FOUND` (il n'y
  a pas de « . » à la racine) ; on le traite comme un dossier vide.

### Libellé de type « txtfile »

- **Constat** : pour `.gitignore`, `SHGetFileInfoW(SHGFI_TYPENAME)` renvoie « txtfile ». La clé
  `HKCR\.gitignore` pointe vers un ProgID `txtfile` qui n'existe pas sur cette machine.
  L'Explorateur affiche **le même libellé** (vérifié via `Shell.Application`, `GetDetailsOf`
  colonne 2). Ce n'est donc pas un bogue de notre côté.

### Automatisation des tests d'interface : verrou de premier plan

- **Constat** : `SetForegroundWindow` depuis un script échoue souvent (verrou de premier plan
  de Windows). Des clics synthétiques envoyés « à l'aveugle » peuvent alors atterrir dans une
  autre application, et `CopyFromScreen` capture ce qui est réellement à l'écran.
- **Parade** : vérifier `GetForegroundWindow()` avant chaque entrée synthétique et capturer par
  `PrintWindow(PW_RENDERFULLCONTENT)`, qui ne photographie que notre fenêtre (Mica compris).
  À terme, préférer des tests d'interface headless (backend de test Slint).

### Manifeste

- Vérification de l'intégration : `mt.exe -inputresource:windir.exe;#1 -out:x.manifest`
  (SDK 10.0.26100). Le manifeste contient PerMonitorV2, `longPathAware`, `asInvoker` et
  Common Controls v6.

### Slint

- Un identifiant d'élément nommé `list` masque la valeur d'énumération `list` :
  `accessible-role: list` échoue alors avec « Cannot take reference of an element ».
- `ensure-visible` est le nom d'une fonction intégrée : la redéfinir provoque un avertissement
  de masquage.
- Le code généré par `slint::include_modules!()` utilise `unwrap()`. On l'inclut dans un
  module annoté `#[allow(clippy::unwrap_used, …)]` pour garder les lints stricts sur notre code.

---

## Phase 2

### Le Shell refuse les barres obliques

- **Constat** : `SHParseDisplayName` et `SHCreateItemFromParsingName` échouent avec
  `E_INVALIDARG` (« Paramètre incorrect ») sur `C:/Users/…`, alors que les API du système de
  fichiers (`CreateFileW`…) acceptent ce chemin. Défaut trouvé par l'auto-test
  (`windir --self-test`) : la création de dossier et le menu contextuel échouaient sur un
  dossier passé avec des `/`.
- **Parade** : tous les appels Shell passent par `fx_win::path::shell_path` (chemin absolu
  normalisé, sans `\?\`), et le chemin de départ est normalisé dans `fx-app`.

### Menu contextuel affiché par un autre thread que celui de la fenêtre

- `TrackPopupMenuEx` fonctionne depuis l'hôte Shell (thread STA à part) avec sa fenêtre cachée
  pour propriétaire, à condition d'attacher les files d'entrée (`AttachThreadInput`) avec le
  thread de la fenêtre principale pendant l'affichage. Le menu se ferme alors normalement au
  clic extérieur.
- Les sous-menus dynamiques (« Envoyer vers », « Ouvrir avec ») ne se remplissent que si la
  fenêtre propriétaire relaie `WM_INITMENUPOPUP`, `WM_DRAWITEM`, `WM_MEASUREITEM` et
  `WM_MENUCHAR` à `IContextMenu3::HandleMenuMsg2` (vérifié par test : 7 entrées).
- Fermeture automatique sans entrée synthétique (diagnostic) : minuterie sur la fenêtre hôte,
  puis `EndMenu()`.

### Constantes absentes du crate `windows` 0.62

- `CMIC_MASK_UNICODE` (0x4000) et `MK_ALT` (0x20) ne sont pas exportées ; elles sont définies
  localement avec leur valeur du SDK.

### Glisser-déposer et boucle winit

- winit enregistre par défaut sa propre cible OLE : on la désactive
  (`with_drag_and_drop(false)` via le hook d'attributs de Slint) pour enregistrer notre
  `IDropTarget`, après `OleInitialize` sur le thread d'interface.
- `SHDoDragDrop` lance une boucle modale **à l'intérieur** d'un callback Slint. winit met alors
  en attente les événements de la fenêtre : notre fenêtre n'est pas redessinée pendant un
  glissement qu'elle a elle-même lancé (le dépôt fonctionne, mais la surbrillance du dossier
  survolé n'apparaît qu'à la fin). OLE capture aussi la souris : on envoie à Slint un
  relâchement synthétique (`dispatch_event`) après le dépôt.
- `IDropTarget` est testé en l'appelant comme le fait OLE, avec un vrai objet de données du
  Shell (`cargo test -p fx-win --test shell drop_target`).

### `IShellItemImageFactory` renvoie de l'alpha prémultiplié

- Les icônes de la liste d'images système (`IImageList::GetIcon`) sont en alpha droit ; les
  bitmaps de `IShellItemImageFactory::GetImage` sont prémultipliés. `fx_win::icons` convertit
  les deux vers de l'alpha droit pour Slint.

---

## Phase 3

### Partage réseau injoignable : 42 s de blocage, annulable

- **Constat** : `CreateFileW` sur `\10.255.255.1\partage` (adresse sans réponse) bloque environ
  **42 s** avant `ERROR_BAD_NETPATH`. `CancelSynchronousIo` sur le thread bloqué le libère
  aussitôt avec `ERROR_OPERATION_ABORTED` (300 ms dans le test, le temps d'appeler l'annulation).
- **Cache négatif du redirecteur SMB** : après un échec, un nouvel accès au même serveur échoue
  instantanément pendant un certain temps. Pour mesurer le blocage, il faut une adresse jamais
  essayée.
- **Adresse de documentation** : `192.0.2.1` échoue instantanément (aucune route) ; elle ne
  convient pas pour tester un blocage.
- **Reproduction** : `cargo test -p fx-win --test listing unreachable -- --nocapture --include-ignored`.
- **Conséquence** : le chargeur garde le `JoinHandle` du thread de listing et appelle
  `CancelSynchronousIo` à l'annulation (Échap, changement de dossier, délai dépassé).

### La surveillance ouvrait le dossier sur le thread d'interface

- Défaut de la phase 2 : `watch()` (donc `CreateFileW` sur le dossier) était appelé depuis
  `navigate()`, sur le thread d'interface. Sur un partage injoignable, l'interface aurait gelé
  42 s. La surveillance démarre maintenant sur le thread de listing et arrive avec l'événement
  `Complete`.

### Pièges d'outillage (environnement de développement)

- L'outil shell utilisé pour piloter le développement (bash MSYS) réduit `\` en `\` dans les
  arguments et dans certains heredocs : un chemin UNC y devient `\serveur\…`, interprété comme
  `C:\serveur\…`. Les chemins UNC de test sont donc écrits avec l'éditeur, et les essais passent
  par PowerShell.
- PowerShell : `… | Select-Object -First 1` interrompt le processus et laisse `$LASTEXITCODE` à
  255 ; ce n'est pas le code de sortie de l'application.

## Phase 4

- **Prévisualiseurs (`IPreviewHandler`)** : le CLSID se trouve par
  `AssocQueryStringW(ASSOCSTR_SHELLEXTENSION, ".ext", "{8895b1c6-b41f-4c1c-a562-0d564250836f}")`
  (avec `ASSOCF_INIT_DEFAULTTOSTAR`). L'initialisation essaie, dans l'ordre,
  `IInitializeWithFile`, `IInitializeWithItem`, `IInitializeWithStream` : chaque prévisualiseur
  n'en implémente qu'une partie. `CLSCTX_LOCAL_SERVER` est accepté en premier : le
  prévisualiseur PDF d'Edge (`{3A84F9C2-…}`) tourne alors hors processus, et un plantage ne
  touche pas WinDir.
- La fenêtre enfant qui héberge l'aperçu est créée **par le thread STA de l'hôte d'aperçu**,
  pas par le thread d'interface : un prévisualiseur lent ne bloque pas l'interface (latence
  maximale mesurée pendant l'auto-test : 6,4 ms). Une fenêtre enfant d'un autre thread relie
  les files d'entrée des deux threads ; aucun appel synchrone n'est fait du thread d'interface
  vers la fenêtre enfant.
- Les éléments Slint dessinés par-dessus la zone d'aperçu (palette, suggestions) sont masqués
  par la fenêtre enfant, qui est une vraie fenêtre Win32 au-dessus de la surface de rendu.
- `PrintWindow(PW_RENDERFULLCONTENT)` ne capture pas le contenu d'un prévisualiseur hors
  processus (zone grise) ; une capture de l'écran le montre bien. Il faut ~1 s au
  prévisualiseur PDF d'Edge pour son premier rendu.
- Vignettes : `IShellItemImageFactory::GetImage` avec `SIIGBF_THUMBNAILONLY` évite de recevoir
  une icône à la place d'une vignette ; `SIIGBF_INCACHEONLY` ne lit que le cache du Shell. Un
  fichier « en ligne seulement » n'est jamais présenté au Shell pour un aperçu (il serait
  téléchargé).
- Saisie au clavier : deux touches à moins d'une seconde d'intervalle forment une seule
  recherche (« a » puis « i » cherche « ai »), comme dans l'Explorateur ; l'auto-test réinitialise
  la saisie entre deux sélections.
- **Icônes 256 px** : `SHGetImageList(SHIL_JUMBO)` dessine une icône sans version 256 px en
  petit dans le coin supérieur gauche d'un carré de 256 px. On mesure l'étendue des pixels non
  transparents ; si elle tient dans 64 px, on reprend la liste `SHIL_EXTRALARGE` (48 px).
- **Vue icônes et Slint** (`ListView` de lignes de tuiles, chaque ligne portant un modèle de
  tuiles) :
  - remplacer une ligne (`row_changed`) recrée son modèle interne, donc réinstancie toutes ses
    tuiles. Une icône livrée ou un changement de sélection met donc à jour la tuile en place
    (`VecModel::set_row_data` sur le modèle de la ligne, gardé en cache) ;
  - notifier le modèle pendant que Slint lit `row_data` provoque une panique
    (`RefCell already borrowed` dans `repeater.rs`) : aucune notification depuis `row_data` ;
  - une livraison d'icônes ne doit rafraîchir que les éléments qui attendaient ces icônes
    (attente indexée par clé) ; tout rafraîchir à chaque lot coûtait ~2 400 mises à jour par
    image en plein écran ;
  - ne demander d'icône que pour les lignes visibles : sinon les requêtes au-delà de la file
    du thread d'icônes sont annulées, redemandées, annulées… (CPU à 100 %, plus de rendu).
- Les icônes sont réduites à leur taille d'affichage dans le thread d'icônes : le moteur
  logiciel de Slint redimensionnerait sinon l'image à chaque image (sans effet mesurable ici,
  mais moins de mémoire : 96 px au lieu de 256 px).
