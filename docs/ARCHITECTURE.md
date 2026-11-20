# Architecture

Explorateur de fichiers pour Windows 11, écrit en Rust, conçu pour être plus rapide et plus
léger que l'Explorateur Windows. Ce document décrit l'organisation cible. Les choix
structurants sont justifiés dans les ADR :

- [ADR 0001 — Framework d'interface](adr/0001-framework-ui.md) : Slint fluent, moteur de rendu logiciel
- [ADR 0002 — Méthode de listing](adr/0002-methode-listing.md) : `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)` (accepté, mesuré)
- [ADR 0003 — Stratégie d'index](adr/0003-strategie-index.md) : MFT + USN dans un service optionnel
- [ADR 0004 — Stratégie de panic](adr/0004-strategie-panic.md) : `panic = "unwind"`, workers isolés par `catch_unwind`

## 1. Principes

1. **Le thread d'interface ne fait que de l'affichage.** Aucune E/S disque ou réseau, aucun appel
   Shell potentiellement bloquant n'y est exécuté.
2. **Tout est incrémental et annulable.** Les résultats arrivent par lots ; quitter un dossier
   annule tout ce qui le concerne.
3. **On ne matérialise que ce qui est visible.** Le modèle garde des entrées compactes ; le
   texte affiché, les icônes et les miniatures ne sont produits que pour les lignes visibles.
4. **`unsafe` confiné dans `fx-win`**, derrière des API sûres, chaque bloc justifié par
   `// SAFETY:`.
5. **Mesurer avant d'affirmer.** Chaque cible de performance a un benchmark ou une
   instrumentation reproductible (section 9).

## 2. Découpage en crates

```
fx-app ──► fx-ui ──► fx-core ◄── fx-index (client)
  │          │          ▲
  │          └──────────┤
  └──────────► fx-win ◄─┘          fx-indexd (service, phase 5) ──► fx-win
```

| Crate | Rôle | Dépend de |
|---|---|---|
| `fx-win` | **Seul crate avec `unsafe`.** Wrappers sûrs Win32/COM/Shell : listing, attributs, chemins `\\?\`, `ReadDirectoryChangesW`, `IFileOperation`, `IContextMenu`, icônes, DWM, presse-papiers, OLE drag-drop, threads STA. | `windows` |
| `fx-core` | Modèle métier sans interface : dossier et entrées, tri naturel, filtres, navigation et historique, application des événements de surveillance, file d'opérations. Testable sans fenêtre ; le système de fichiers est abstrait par des traits (`DirLister`, `Watcher`, `FileOps`). | — (traits) ; `fx-win` derrière une feature `windows` |
| `fx-index` | Client de l'index (named pipe), repli sans élévation, format d'instantané partagé avec le service. | `fx-core`, `fx-win` |
| `fx-ui` | Fichiers `.slint`, adaptateurs `slint::Model` vers `fx-core`, raccourcis, thème. | `fx-core`, `fx-win` (formatage des dates, DWM, Shell), `slint` |
| `fx-app` | Binaire : manifeste, démarrage, configuration TOML, journalisation `tracing`, `--bench-startup`. | tous |
| `benches/` | Benchmarks `criterion` (listing, tri, filtre, requête d'index). | `fx-core`, `fx-win` |

## 3. Modèle de threads

```
┌───────────────────────────── processus fx-app ──────────────────────────────┐
│ Thread UI (STA, boucle winit/Slint)                                          │
│   affichage, saisie, adaptateurs Model, OLE drag-drop (RegisterDragDrop)     │
│        ▲  slint::invoke_from_event_loop(lots, générations)                  │
│        │                                                                     │
│ Pool « E/S » (MTA, N = cœurs)       Threads « Shell » (STA, pompe messages)  │
│   listing, tri parallèle (rayon),     icônes / miniatures,                   │
│   métadonnées, taille de dossiers     IContextMenu, IFileOperation,          │
│                                       IPreviewHandler                        │
│ Thread « surveillance »                                                      │
│   ReadDirectoryChangesW (overlapped), regroupement 50 ms                     │
└──────────────────────────────────────────────────────────────────────────────┘
```

- Les workers communiquent vers l'interface par `slint::invoke_from_event_loop` (pas de
  scrutation), et reçoivent leurs requêtes par canaux `crossbeam-channel`.
- Les appels Shell qui exigent un STA tournent sur des threads dédiés initialisés en
  `COINIT_APARTMENTTHREADED`, avec leur propre pompe de messages et une fenêtre propriétaire
  cachée pour les menus.
**État en phase 5** — threads réellement utilisés :

| Thread | Rôle |
|---|---|
| interface (STA OLE) | boucle Slint/winit, `IDropTarget`, source de glisser-déposer (`SHDoDragDrop`, exigé par OLE) |
| `fx-list` (un par chargement, STA) | ouverture de la surveillance puis listing, types, tri initial ; annulable même bloqué dans le noyau (`CancelSynchronousIo`) |
| pool `rayon` | tri, filtre, application des événements de surveillance (COM initialisé à la demande) |
| `fx-watch` (un par dossier affiché) | `ReadDirectoryChangesW`, regroupement, lecture des entrées modifiées |
| `fx-shell` (hôte Shell, STA OLE + fenêtre cachée) | menu contextuel, presse-papiers (propriétaire), ouverture de fichiers |
| `fx-icon-0`, `fx-icon-1` (STA) | icônes et vignettes des éléments affichés |
| `fx-home` (à l'affichage de l'accueil) | icônes et fichiers récents (raccourcis `.lnk` lus sans ouvrir les cibles), COM STA |
| processus `explorermev --hotkey-agent` (si l'option « explorateur par défaut » est active) | crochet clavier bas niveau : Win+E ouvre ExplorerMev ; instance unique ; lancé par la clé `Run` de l'utilisateur |
| `fx-search` (dès la première recherche) | possède les index de recherche : requêtes (seule la plus récente est exécutée, balayage parallèle `rayon`), changements, cache, libération après 5 min d'inactivité |
| `fx-search-build` + pool `fx-index-N` (priorité arrière-plan, moitié des cœurs) | construction d'un index par parcours des dossiers |
| `fx-watch-tree` (un par volume indexé) | surveillance récursive de la racine du volume (noms et attributs) |
| `fx-mft` (sur demande) | tube vers l'assistant élevé ; relaie ses trames au moteur |
| processus `explorermev --index-helper` (élevé, sur demande) | lit la MFT puis le journal USN, filtre par droits d'accès, écrit dans le tube ; ne lit rien |
| `fx-preview` (hôte d'aperçu, STA OLE) | prévisualiseurs du Shell (`IPreviewHandler`) dans une fenêtre enfant, vignettes du volet d'aperçu |
| `fx-preview-text` (un par aperçu) | lecture des 64 premiers Kio d'un fichier texte |
| pool `rayon` (taille de dossiers) | parcours récursif annulable, sans suivre les points de réanalyse |
| `fx-fileop` (un par opération, STA OLE) | `IFileOperation`, collage |
| `fx-latency` (modes de mesure seulement) | sonde de latence du thread d'interface |
| `fx-nav`, `fx-suggest` | volet de navigation (noms des lecteurs, délai de 1,5 s par lecteur), autocomplétion |

- Chaque worker s'exécute sous `catch_unwind` (avec `panic = "unwind"` dans les crates de
  bibliothèque ; voir section 8). Un panic fait échouer la requête, est journalisé et ne
  touche pas l'état partagé : les workers ne partagent que des messages, aucun verrou exposé.

## 4. Ouverture d'un dossier

1. L'interface incrémente la **génération** de l'onglet (`u64`) et annule le jeton de la
   génération précédente (`Arc<AtomicBool>`, consulté entre deux lots par le listing, les
   icônes et les miniatures).
2. Une requête `List { path, gen, token }` part vers le pool E/S. Elle peut partir **avant la
   création de la fenêtre** au démarrage, pour que le listing et l'initialisation de
   l'interface se recouvrent.
3. Le listing (ADR 0002) envoie un premier lot dès qu'il a environ 2 000 entrées ou après 16 ms,
   puis des lots plus gros. L'interface affiche le premier lot non trié (ordre de l'index NTFS,
   déjà alphabétique) pendant le tri complet.
4. Le tri complet (naturel, dossiers en premier, `rayon` au-delà de 20 000 entrées) produit
   un vecteur d'ordre `Vec<u32>` ; l'interface remplace son ordre en une fois.
5. La surveillance démarre sur le dossier ; ses événements, regroupés, sont appliqués en
   différentiel (insertion ou suppression dans l'ordre trié par recherche dichotomique).
6. Tout résultat dont la génération diffère de celle de l'onglet est ignoré.

## 5. Représentation des données

```rust
/// 48 octets par entrée (vérifié par test) ; 100 000 entrées ≈ 4,8 Mo + noms.
pub struct Entry {
    pub size: u64,
    pub modified: i64,    // FILETIME brut, formaté seulement à l'affichage
    pub created: i64,
    name_start: u32,      // décalage dans l'arène UTF-16 du dossier
    pub attributes: u32,  // dont RECALL_ON_DATA_ACCESS, RECALL_ON_OPEN, PINNED, UNPINNED
    pub reparse_tag: u32, // lien symbolique, jonction, cloud…
    pub type_id: u32,     // index dans la table des libellés de type du dossier
    name_len: u16,
}
pub struct Folder {       // partagé en Arc<Folder> entre le chargeur et l'interface
    path: PathBuf,        // un seul chemin complet pour toutes les entrées
    names: Vec<u16>,      // arène UTF-16 (noms invalides conservés tels quels)
    entries: Vec<Entry>,
    types: Vec<Arc<str>>, // libellés de type, partagés par extension
}
// Côté interface : order: Arc<Vec<u32>> (tri), visible: Vec<u32> (filtre),
// clés de tri des noms en arènes par blocs (NameKeys), recalculées une fois par chargement.
```

- Pas de `PathBuf` par entrée. Le chemin complet n'est construit (`OsString::from_wide`) qu'au
  moment d'agir sur un fichier.
- Le libellé de type, l'icône générique et la clé de tri naturel sont partagés par extension.
- L'adaptateur `slint::Model` ne fabrique un `Row` (chaînes formatées) que dans `row_data`,
  donc pour les lignes visibles. Le prototype de la phase 0 a confirmé qu'environ 20 lignes
  seulement sont instanciées pour 100 000 entrées.

## 6. Chemins, liens et fichiers cloud

- Tous les appels Win32 passent par des chemins `\\?\` (ou `\\?\UNC\`) construits par `fx-win`,
  et le manifeste déclare `longPathAware`.
- Les liens symboliques et les jonctions sont identifiés par `reparse_tag` sans les suivre. La
  navigation dans un lien est explicite ; les parcours récursifs (taille de dossier, recherche
  de repli) mémorisent les identifiants de fichier visités pour éviter les boucles.
- Fichiers OneDrive : on ne lit jamais de données, d'icône extraite du fichier ni de miniature
  pour une entrée portant `RECALL_ON_DATA_ACCESS` ou `RECALL_ON_OPEN`. On utilise uniquement
  l'icône par type et `SIIGBF_INCACHEONLY`. L'état (en ligne seulement, disponible localement,
  toujours conservé) se déduit des attributs.
- Volumes réseau et amovibles : toute opération a un délai (5 s par défaut) et un jeton
  d'annulation. Un dossier injoignable affiche une erreur dans l'onglet sans bloquer l'interface.

## 7. Interface (résumé de l'ADR 0001)

- Slint 1.19, style `fluent`, backend winit, **moteur de rendu logiciel** par défaut
  (démarrage et mémoire). Option `renderer = "gpu"` (femtovg).
- Mica via `DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW)`, barre de
  titre sombre via `DWMWA_USE_IMMERSIVE_DARK_MODE`, mise à jour sur `WM_SETTINGCHANGE`
  (`ImmersiveColorSet`).
- Vue détails maison : en-tête redimensionnable et `ListView` de lignes de hauteur fixe.
  Sélection multiple, édition de nom et icônes gérées par nos composants.
- Le glisser-déposer intégré de winit est désactivé pour enregistrer notre `IDropTarget`.
- **Onglets et volets** (phase 4) : un `Pane` porte tout l'état d'une vue (dossier, sélection,
  historique, chargeur, surveillance, renommage) ; un `Tab` contient deux volets (le second
  n'est affiché qu'en double volet) ; `App` garde l'état global (onglets, palette, aperçu,
  favoris, session). La fenêtre a deux emplacements de volet ; seuls les volets de l'onglet
  actif y sont rattachés.
- **Vue icônes** : `ListView` de lignes de tuiles (`GridModel`, une ligne = `colonnes`
  éléments du `FolderModel`). Chaque ligne fabriquée garde son modèle de tuiles ; un changement
  d'élément (icône, sélection) met à jour la tuile en place. Les icônes ne sont demandées que
  pour les lignes visibles (plage relevée à chaque image). Vignettes : cache du Shell puis
  génération, jamais pour un fichier en ligne seulement, 200 au plus en mémoire.
- **Aperçu** : texte (64 Kio, UTF-8/UTF-16), images (vignette du Shell), autres types par le
  prévisualiseur enregistré (`IPreviewHandler`), dans une fenêtre enfant créée par le thread
  `fx-preview` et replacée à chaque image si la mise en page change. Délai de 120 ms après le
  dernier changement de sélection.

## 8. Erreurs, robustesse, configuration

- `thiserror` dans les bibliothèques, `anyhow` dans `fx-app`. Les erreurs Win32 gardent leur
  `HRESULT` et sont traduites en message lisible pour l'utilisateur dans `fx-ui`.
- Pas de `unwrap()`/`expect()` hors tests et invariants prouvés (`clippy::unwrap_used` en deny
  hors tests).
- Panics : `panic = "unwind"` en release, workers sous `catch_unwind`, hook de journalisation
  ([ADR 0004](adr/0004-strategie-panic.md) : +2,3 Mo, pas d'effet mesurable au démarrage).
- Journalisation `tracing` vers un fichier tournant dans `%LOCALAPPDATA%\<app>\logs`, niveau
  réglable par la configuration ou par `RUST_LOG`.
- Configuration TOML dans `%APPDATA%\<app>\config.toml`. Un fichier corrompu donne les valeurs
  par défaut plus un avertissement, sans être écrasé ; il est sauvegardé en `.bak` avant toute
  réécriture.

## 9. Mesure

- Horodatages `tracing` aux étapes clés : début de `main`, fenêtre créée, première image,
  premier lot, liste triée.
- `fx-app --bench-startup` : affiche les durées depuis la création du processus
  (`GetProcessTimes`) et quitte.
- Benchmarks `criterion` dans `benches/` sur des jeux de fichiers générés par script.
- Mémoire de référence : `PrivateUsage` ; le working set est aussi rapporté.
- Méthode de mesure du défilement : celle du prototype, décrite dans
  [mesures/phase0-prototype.md](mesures/phase0-prototype.md).

## 10. Risques suivis

| Risque | Phase | Parade |
|---|---|---|
| Rendu logiciel trop coûteux en 4K à haute fréquence | 3 | Mesure en 4K ; limiter les images par seconde ; option `gpu` |
| Glisser-déposer lancé depuis notre fenêtre : pas de rafraîchissement pendant la boucle modale d'OLE | 2 | Limitation connue (NOTES-WIN32.md) |
| Mise à jour du thème à chaud (palette Slint + DWM) | 1 | Test manuel en basculant le thème Windows |
| Divulgation de noms par le service d'index | 5 | Filtrage par emprunt d'identité (ADR 0003) |
