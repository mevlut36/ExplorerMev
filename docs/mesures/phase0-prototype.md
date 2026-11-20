# Mesures de la phase 0 — prototype Slint

Prototype : `prototypes/slint-scroll` (jetable). Reproduction : `prototypes/slint-scroll/bench.ps1`
(rapports bruts dans `target/bench/phase0/`).

## Banc de test

- Ryzen 5 5600X (6 cœurs), 32 Go, Radeon RX 6600 XT (pilote 32.0.21045.29003)
- Windows 11 Pro 10.0.26300, écran 2560×1440 à 165 Hz, échelle 100 %, thème sombre
- Rust 1.100.0, Slint 1.19.0, winit 0.30.13, profil `release` (LTO fat, codegen-units 1)
- Données factices : 100 000 lignes générées à la demande (`Model::row_data`), 2 % de dossiers

## Méthode

- **Démarrage** : temps entre la création du processus (`GetProcessTimes`) et la fin du rendu
  de la première image. Fin du rendu = `AfterRendering` du rendering notifier (femtovg), ou
  borne haute par un timer à délai nul exécuté après `RedrawRequested` (moteur logiciel).
  Démarrage **à chaud** : fichiers déjà en cache. Le démarrage à froid après redémarrage reste à
  mesurer (phase 3).
- **Défilement** : la position est pilotée par un timer de 2 ms selon trois scénarios de
  4 s chacun : *molette* (1 500 px/s), *rapide* (15 000 px/s, environ 470 lignes par seconde
  nouvelles), *sauts* (position aléatoire à chaque tick, l'équivalent d'un glisser sur l'ascenseur, pire cas).
  Chaque image est horodatée. On compte les intervalles supérieurs à 16,7 ms (cible 60 i/s).
- **CPU** : temps noyau + utilisateur du processus sur la phase, en % d'un cœur.
- **Mémoire** : `PROCESS_MEMORY_COUNTERS_EX.PrivateUsage` (mémoire privée, métrique de
  référence) et `WorkingSetSize`, 1 s après la première image.

## Résultats (fenêtre 1100×720 px)

| Moteur / vue | 1ʳᵉ image rendue | Privée / WS au repos | Molette i/s (CPU) | Rapide i/s (CPU) | Sauts i/s (CPU) | Images > 16,7 ms |
|---|---|---|---|---|---|---|
| logiciel / maison | 56–65 ms | 7,0 / 22,8 Mo | 161 (20–30 %) | 161 (27–31 %) | 160 (53–58 %) | 0 |
| logiciel / StandardTableView | 57–98 ms | 7,3 / 23,1 Mo | 160 (33–38 %) | 160 (40 %) | 159–160 (57–65 %) | 0 |
| femtovg / maison | 303–340 ms | 166,6 / 82,2 Mo | 162 (18–20 %) | 161 (19–21 %) | 162 (40–41 %) | 0 |
| femtovg / StandardTableView | ≈ 300 ms | 166,4 / 82,2 Mo | 160 (21 %) | 160 (27 %) | 160 (50 %) | 0 |
| skia / maison | non mesuré | 226,2 / 149,2 Mo | — | — | — | — |

Les plages de démarrage portent sur 5 passages par moteur.

Rendu CPU par image avec femtovg (rendering notifier) : p99 ≤ 2,3 ms, max ≤ 3,1 ms.

## Résultats en plein écran (2560×1369 px)

| Moteur | Privée / WS | Molette i/s (CPU) | Rapide i/s (CPU) | Sauts i/s (CPU) | Intervalle max | Images > 16,7 ms |
|---|---|---|---|---|---|---|
| logiciel | 18,1 / 33,9 Mo | 159 (79 %) | 159 (84 %) | **117–128 (99–100 %)** | 13,1 ms | 0 |
| femtovg | 213,8 / 83,8 Mo | 160 (24 %) | 160 (24 %) | 159 (59 %) | 10,1 ms | 0 |

## Lecture face aux cibles

| Cible | Résultat du prototype | Verdict |
|---|---|---|
| Démarrage < 250 ms | logiciel 56–65 ms ; femtovg 303–340 ms | atteint avec le moteur logiciel (à chaud, sans données réelles) |
| Mémoire < 60 Mo au repos | logiciel 7 Mo privés ; femtovg 166 Mo | atteint avec le moteur logiciel, avec une marge de 50 Mo pour le modèle, les icônes et les caches |
| Défilement à 60 i/s constants | ≥ 117 i/s au pire, 0 image > 16,7 ms, tous moteurs | atteint, avec réserve en 4K pour le moteur logiciel (non mesuré) |

## Limites de ces mesures

- Une seule machine, un seul écran, échelle 100 %. À refaire en 4K à 150 % et sur une machine
  modeste (portable 4 cœurs, iGPU) en phase 3.
- Le moteur logiciel est horodaté au début de chaque image (`RedrawRequested`), pas à sa
  présentation. Femtovg l'est à la fin du rendu, avant l'échange des tampons.
- Données factices : le coût de formatage est inclus (dates, tailles), mais pas les icônes ni
  l'accès au modèle réel.
- Démarrage à chaud seulement.
