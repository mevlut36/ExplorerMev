# Architecture

Explorateur de fichiers pour Windows 11, écrit en Rust, conçu pour être plus rapide et plus
léger que l'Explorateur Windows. Ce document décrit l'organisation cible. Les choix
structurants sont justifiés dans les ADR :

- [ADR 0001 — Framework d'interface](adr/0001-framework-ui.md) : Slint fluent, moteur de rendu logiciel
- [ADR 0002 — Méthode de listing](adr/0002-methode-listing.md) : `GetFileInformationByHandleEx`, à confirmer par benchmark
- [ADR 0003 — Stratégie d'index](adr/0003-strategie-index.md) : MFT + USN dans un service optionnel

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
| `fx-ui` | Fichiers `.slint`, adaptateurs `slint::Model` vers `fx-core`, raccourcis, thème. | `fx-core`, `slint` |
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
/// ~40 octets par entrée ; 100 000 entrées ≈ 4 Mo + noms.
struct Entry {
    name: NameRef,        // u32 décalage + u16 longueur dans l'arène UTF-16 du dossier
    size: u64,
    modified: i64,        // FILETIME brut, formaté seulement à l'affichage
    created: i64,
    attributes: u32,      // dont RECALL_ON_DATA_ACCESS, RECALL_ON_OPEN, PINNED, UNPINNED
    reparse_tag: u32,     // lien symbolique, jonction, cloud…
    type_id: u16,         // index dans la table des types (extension → libellé, icône)
    flags: u16,           // dossier, caché, système, nom non convertible…
}
struct Folder {
    path: FolderPath,     // un seul chemin complet pour toutes les entrées
    names: Vec<u16>,      // arène UTF-16 (noms invalides conservés tels quels)
    entries: Vec<Entry>,
    order: Vec<u32>,      // vue triée
    filtered: Option<Vec<u32>>,
}
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

## 8. Erreurs, robustesse, configuration

- `thiserror` dans les bibliothèques, `anyhow` dans `fx-app`. Les erreurs Win32 gardent leur
  `HRESULT` et sont traduites en message lisible pour l'utilisateur dans `fx-ui`.
- Pas de `unwrap()`/`expect()` hors tests et invariants prouvés (`clippy::unwrap_used` en deny
  hors tests).
- `panic = "abort"` est demandé pour le binaire. Or `catch_unwind` exige `panic = "unwind"` :
  **conflit**. Résolution retenue pour la phase 1 : profil release en `panic = "unwind"` (coût
  mesuré sur la taille du binaire et le démarrage), workers sous `catch_unwind`, plus un hook
  de panic qui journalise. Si la mesure montre un coût notable, on revient à `abort`, ce qui
  impose que les workers ne paniquent jamais. Décision à consigner dans un ADR en phase 1.
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
| Conflit `panic = "abort"` / isolation des panics | 1 | Mesure puis ADR (section 8) |
| Interopérabilité drag-drop et menus Shell avec la boucle winit | 2 | Prototype isolé au début de la phase 2 |
| Mise à jour du thème à chaud (palette Slint + DWM) | 1 | Test manuel en basculant le thème Windows |
| Divulgation de noms par le service d'index | 5 | Filtrage par emprunt d'identité (ADR 0003) |
