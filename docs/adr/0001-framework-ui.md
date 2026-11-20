# ADR 0001 — Framework d'interface : Slint (style fluent), moteur de rendu logiciel par défaut

- **Statut** : accepté (phase 0, 2026-11-20)
- **Décideurs** : équipe projet
- **Mesures** : [docs/mesures/phase0-prototype.md](../mesures/phase0-prototype.md)

## Contexte

L'explorateur doit démarrer en moins de 250 ms, consommer moins de 60 Mo au repos et défiler
à 60 images/s constantes dans un dossier de 100 000 éléments, avec une apparence Windows 11
(Mica, thème système, Segoe UI Variable, Segoe Fluent Icons).

Le cahier des charges désigne Slint (style `fluent`) avec repli sur egui/eframe si Slint ne
tient pas le défilement ou ne permet pas Mica. Le prototype `prototypes/slint-scroll`
(Slint 1.19.0, backend winit 0.30) a mesuré trois moteurs de rendu Slint et deux vues.

## Constats mesurés

Machine : Ryzen 5 5600X, Radeon RX 6600 XT, écran 2560×1440 à 165 Hz, échelle 100 %.
Fenêtre de 1100×720 px sauf mention contraire. Démarrage à chaud (fichiers en cache).

| Moteur | Démarrage (1ʳᵉ image rendue) | Mémoire privée au repos | Défilement 100 000 lignes |
|---|---|---|---|
| **logiciel** | **56–65 ms** | **7 Mo** (18 Mo en plein écran) | 160 i/s, 0 image > 16,7 ms ; plein écran 2560×1369 : 117–128 i/s au pire, 0 image > 16,7 ms |
| femtovg (OpenGL) | 303–340 ms | 166 Mo (214 Mo en plein écran) | 160 i/s, 0 image > 16,7 ms, rendu ≤ 3 ms |
| skia (Direct3D) | non mesuré | 226 Mo | non mesuré (voir NOTES-WIN32) |

- Le surcoût mémoire et le démarrage lent des moteurs GPU viennent du pilote graphique
  chargé dans le processus (pilote OpenGL AMD : environ 160 Mo privés). Ces moteurs ratent à
  eux seuls deux cibles (démarrage et mémoire).
- Mica fonctionne avec les trois moteurs : `DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE,
  DWMSBT_MAINWINDOW)` sur la fenêtre transparente créée par winit suffit.
- `StandardTableView` n'est pas plus rapide que la vue maison (`ListView` + lignes dessinées) et
  n'accepte que des cellules texte avec une seule ligne courante : pas d'icônes, pas de
  sélection multiple, pas d'édition en place.

## Décision

1. **Slint 1.19, style `fluent`, backend winit.** Le repli sur egui n'est pas déclenché.
2. **Moteur de rendu logiciel par défaut.** Un réglage de configuration
   (`renderer = "gpu"`) permet de choisir femtovg pour les très grandes surfaces (4K, haute
   fréquence) au prix de la mémoire du pilote.
3. **Vue détails maison** : en-tête de colonnes redimensionnables et `ListView` de lignes de
   hauteur fixe, alimentée par un `slint::Model` paresseux qui ne construit que les lignes
   visibles (20 lignes instanciées pour 100 000 éléments dans le prototype).

## Conséquences

- Le rendu logiciel est monocœur. En défilement continu en plein écran à 165 Hz, il occupe
  80 à 100 % d'un cœur. Le risque est réel sur un écran 4K à 150 % (environ 2,4 fois plus de
  pixels). À traiter en phase 3 : mesure en 4K, limitation du nombre d'images par seconde
  pendant les animations si nécessaire, réglage `gpu` documenté.
- Pas de rendering notifier avec le moteur logiciel : l'instrumentation des images passe par
  l'événement winit `RedrawRequested`.
- Il faudra désactiver le glisser-déposer intégré de winit (`with_drag_and_drop(false)` via
  le hook d'attributs de fenêtre) pour enregistrer notre propre `IDropTarget` (phase 2).
- Taille du binaire du prototype : 10,4 Mo avec les deux moteurs. Le binaire final
  n'embarquera femtovg que si le réglage `gpu` est conservé.

## Alternatives écartées

- **egui/eframe** : non nécessaire. Le rendu immédiat redessine tout à chaque image, et il
  faudrait recréer tous les contrôles Fluent.
- **Skia** : pire empreinte mémoire mesurée (226 Mo) et binaire de 19,5 Mo.
- **`StandardTableView`** : fonctionnellement insuffisant (voir constats).
