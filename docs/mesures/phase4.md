# Mesures de la phase 4 — fonctions avancées

Même banc que la phase 3 : Ryzen 5 5600X, Radeon RX 6600 XT, Windows 11 Pro, écran
2560×1440 à 165 Hz, échelle 100 %. Profil `release`, moteur de rendu logiciel. Les modes de
mesure ignorent la configuration de l'utilisateur et ne restaurent pas la session.

## Cibles, avec l'interface de la phase 4 (onglets, double volet, aperçu, vue icônes)

| Cible | Mesure | Résultat | Verdict |
|---|---|---|---|
| Démarrage < 250 ms | `--bench-startup` ×5, cache chaud | 73,9–83,4 ms (première image) | atteint |
| Démarrage à froid < 250 ms | exécutable recopié sans cache (`xcopy /J`) ×3 | 78,1–79,3 ms (rendu ≤ 84,6 ms) | atteint (approché) |
| Mémoire < 60 Mo (1 onglet, dossier de 1 000 éléments) | `--bench-open n1000` | 16,7 Mo privés / 40,6 Mo WS | atteint |
| Idem en vue icônes | `--bench-open n1000 --icons` | 22,8 Mo privés / 54,5 Mo WS | atteint |
| 100 000 fichiers : premier affichage < 150 ms | `--bench-open n100000` ×3 | premières lignes peintes en 9,2–16,2 ms | atteint |
| 100 000 fichiers : liste triée < 1 s | idem | peinte en 61,0–72,3 ms | atteint |
| Défilement à 60 i/s (vue détails) | `--bench-scroll n100000` ×2, fenêtre et plein écran | 148–161 i/s, **0** image > 16,7 ms | atteint |
| Défilement à 60 i/s (vue icônes, fenêtre 1200×760) | `… --icons` ×2 | 159–161 i/s, **0** image > 16,7 ms | atteint |
| Défilement à 60 i/s (vue icônes, plein écran 2560×1369) | `… --icons --maximized` ×2 | molette et rapide : 0 image > 16,7 ms ; sauts aléatoires : 81 i/s, 3 à 5 images > 16,7 ms sur ~325 | atteint sauf sauts aléatoires en plein écran |
| Thread d'interface jamais bloqué | sonde de latence, tous les modes | max 12,3 ms, **0** mesure > 50 ms | atteint |

## Défilement (100 000 éléments)

| Scénario | Détails, fenêtre | Détails, plein écran | Icônes, fenêtre | Icônes, plein écran |
|---|---|---|---|---|
| molette (1 500 px/s) | 160,3–160,5 i/s, p99 6,7 ms | 160,4 i/s, p99 6,7 ms | 160,4–160,6 i/s, p99 6,7 ms | 150,6–151,5 i/s, p99 9,4 ms |
| rapide (15 000 px/s) | 158,9–159,0 i/s, p99 6,7 ms | 160,2–160,7 i/s, p99 6,8 ms | 159,4–159,6 i/s, p99 6,7 ms | 138,0–140,4 i/s, p99 9,5 ms |
| sauts aléatoires | 160,0–160,3 i/s, p99 6,7 ms | 148,7–148,9 i/s, p99 9,1 ms | 159,4–159,6 i/s, p99 6,8 ms | 81,1–81,6 i/s, p99 18,2 ms |

En plein écran, la vue icônes affiche ~200 tuiles. Chaque saut aléatoire en instancie autant
de nouvelles (icône, vignette demandée, nom sur deux lignes) : ~12 ms par image, à 100 % d'un
cœur. Un défilement réel (molette, glisser de l'ascenseur, Page suiv.) ne remplace qu'une partie
des lignes et reste sans image lente.

### Corrections faites pendant la mesure de la vue icônes

| Problème (mesuré) | Correction | Sauts, icônes, plein écran |
|---|---|---|
| Une icône livrée remplaçait toute la ligne de tuiles : Slint réinstanciait les 9 tuiles, jusqu'à 35 fois par ligne et par image | mise à jour de la tuile en place dans le modèle de la ligne | blocage complet (voir ligne suivante) |
| Ces mises à jour demandaient les icônes des lignes hors écran ; la file du thread d'icônes les annulait, elles étaient redemandées, etc. (CPU 100 %, plus de rendu) | icônes demandées pour les lignes visibles seulement | 20,6 i/s |
| Chaque lot d'icônes livré rafraîchissait toutes les tuiles en attente, pas seulement les siennes | attente indexée par clé d'icône | 78–82 i/s |

En fenêtre, les sauts passent de 49 i/s (152 images lentes) à 159 i/s (0).

**Biais du banc corrigé** : le banc de défilement lisait la hauteur maximale à chaque tick
(2 ms), ce qui forçait Slint à mettre la liste à jour entre deux images. Il la lit désormais une
seule fois. La vue détails n'en dépendait pas : mêmes résultats qu'en phase 3.

**Conditions** : une première série, faite pendant une forte activité audio de la machine
(`audiodg`), montrait des images lentes (jusqu'à 265 ms) avec un CPU bas, **y compris avec le
binaire d'avant la vue icônes**. Les chiffres ci-dessus viennent d'une série refaite sur une
machine calme (`audiodg` : 0 s de CPU pendant la série).

## Fonctions de la phase 4 vérifiées dans l'application

Auto-test (`--self-test`) : **30 vérifications sur 30**, latence maximale de l'interface 2,7 ms.
Aux 16 de la phase 3 s'ajoutent :

| Fonction | Vérification |
|---|---|
| Onglets | Ctrl+T ouvre un onglet sur le même dossier ; Ctrl+W le ferme ; Ctrl+Maj+T le rouvre avec son double volet |
| Double volet | F6 l'active ; F5 copie la sélection vers l'autre volet (fichier créé sur le disque) |
| Aperçu texte | `.txt` de 3 000 caractères affiché (lecture des 64 premiers Kio hors du thread d'interface) |
| Aperçu image | vignette du Shell d'un `.bmp` (96×64) |
| Aperçu PDF | prévisualiseur du Shell (Edge, `{3A84F9C2-…}`, hors processus) affiché dans le volet ; contrôlé par capture d'écran |
| Fermeture de l'aperçu | masquer le volet détruit la fenêtre du prévisualiseur |
| Vue icônes | Ctrl+Maj+V ; vignette de l'image affichée dans sa tuile ; Droite → élément suivant, Bas → même colonne, ligne suivante |

Captures (fenêtre de l'application seulement) : double volet, aperçu PDF, vue icônes.

## Option « Remplacer Win+E »

Testée sur une clé de test de `HKCU` (activation, mise à jour, désactivation, refus si un autre
programme a pris Win+E, désactivation sans effet sur la clé d'un autre programme), supprimée
ensuite. **Non activée sur la machine de développement** (désactivée par défaut, activation
réservée à l'utilisateur) : le lancement effectif d'ExplorerMev par Win+E n'a donc pas été observé.
`explorermev --win-e status` confirme l'état d'origine (« Explorateur »).

## Tests

`cargo test --workspace` : 97 réussis, 3 ignorés (presse-papiers, OneDrive, délai SMB).

## Limites

- Mêmes limites que la phase 3 : démarrage à froid approché, une seule machine, échelle 100 %.
- Aperçu : un seul prévisualiseur natif essayé (PDF d'Edge) ; les prévisualiseurs Office ne sont
  pas installés sur la machine de développement.
- Vignettes sur fichiers OneDrive « en ligne seulement » : jamais demandées (icône de type), mais
  pas vérifié sur données réelles (aucun fichier de ce type sur la machine, voir phase 3).
