# Mesures de la phase 3 — vérification des cibles

Banc de test : Ryzen 5 5600X, Radeon RX 6600 XT, Windows 11 Pro, écran 2560×1440 à 165 Hz,
échelle 100 %. Profil `release`, moteur de rendu logiciel. Tous les modes de mesure ignorent la
configuration de l'utilisateur et rapportent la **latence du thread d'interface** : une sonde
lui soumet une tâche toutes les 50 ms et mesure le délai avant son exécution.

## Bilan face aux cibles

| Cible | Mesure | Résultat | Verdict |
|---|---|---|---|
| Démarrage < 250 ms | `--bench-startup`, cache chaud | 63–121 ms (première image) | atteint |
| Démarrage à froid < 250 ms | exécutable copié sans cache (`xcopy /J`) avant chaque lancement | 70–77 ms (rendu ≤ 86 ms) | atteint (approché, voir limites) |
| Mémoire < 60 Mo (dossier de 1 000 éléments) | `--bench-open n1000` | 15,8 Mo privés / 43,6 Mo WS | atteint |
| 100 000 fichiers : premier affichage < 150 ms | `--bench-open n100000` | premières lignes peintes en 9,4–15,9 ms | atteint |
| 100 000 fichiers : liste triée < 1 s | idem | peinte en 59–67 ms | atteint |
| Défilement à 60 i/s constants | `--bench-scroll n100000`, fenêtre 1200×760 | 159–160 i/s, **0** image > 16,7 ms | atteint |
| Idem, plein écran 2560×1369 | `--bench-scroll … --maximized` | 140–158 i/s, **0** image > 16,7 ms | atteint |
| Thread d'interface jamais bloqué | sonde de latence, tous les modes | max 0,4 à 18,3 ms, **0** mesure > 50 ms | atteint |
| Recherche < 50 ms | — | phase 5 | — |

## Défilement dans l'application (100 000 éléments, icônes réelles)

| Scénario | Fenêtre 1200×760 | Plein écran 2560×1369 |
|---|---|---|
| molette (1 500 px/s) | 160,1 i/s, p99 6,64 ms, CPU 22 % | 158,2 i/s, p99 7,62 ms, CPU 67 % |
| rapide (15 000 px/s) | 159,9 i/s, p99 6,67 ms, CPU 30 % | 156,9 i/s, p99 11,32 ms, CPU 74 % |
| sauts aléatoires | 159,7 i/s, p99 6,71 ms, CPU 55 % | 140,5 i/s, p99 8,37 ms, CPU 102 % |

CPU en % d'un cœur. Avant correction, les sauts consommaient 211 % (fenêtre) et 282 % (plein
écran) : les threads d'icônes extrayaient les icônes propres (`.exe`, `.lnk`) de lignes déjà
sorties de l'écran. Ces icônes attendent désormais 60 ms de stabilité, et seules les 64 demandes
les plus récentes sont gardées.

## Réseau

| Situation | Mesure |
|---|---|
| Listing d'un partage injoignable sans annulation (`\10.255.255.1\partage`) | bloqué **42 s** dans le noyau, puis « réseau injoignable » |
| Même listing annulé par `CancelSynchronousIo` à 300 ms | rendu en 300,5 ms (`ERROR_OPERATION_ABORTED`) |
| Dans l'application (`--network-timeout 2`) | message « Cet emplacement ne répond pas » après 2 s ; latence max de l'interface 0,4 ms pendant tout le scénario |

## Auto-test fonctionnel (`--self-test`)

16 vérifications sur 16 : les 11 de la phase 2, plus fichier caché masqué par défaut, Ctrl+H,
partage injoignable abandonné après le délai, fin du chargement, retour arrière.

## OneDrive

Le dossier OneDrive de la machine de développement ne contient que 3 fichiers, dont aucun « en
ligne seulement » : l'absence de téléchargement n'a donc **pas pu être vérifiée sur données
réelles**. La classification a été vérifiée sur ces fichiers (2 « toujours conservés » reconnus,
comme l'indiquent les attributs) ; elle est par ailleurs testée unitairement pour les quatre
états. Par construction, le listing et les icônes de type n'ouvrent jamais les fichiers, et les
icônes propres ne sont jamais extraites d'un fichier en ligne seulement.

## Tests

`cargo test --workspace` : 90 réussis, 3 ignorés (presse-papiers, exécuté une fois avec
sauvegarde et restauration du presse-papiers ; OneDrive, dépendant du poste ; délai SMB, lent).

## Limites

- Démarrage « à froid » approché : seul l'exécutable est relu depuis le disque ; les DLL
  système restent en cache, ce qui correspond à un démarrage après installation et non après
  un redémarrage. Vider le cache système exige des droits d'administration.
- Ouverture de dossier à froid non mesurée (métadonnées NTFS en cache).
- Une seule machine, échelle 100 % ; pas de mesure en 4K à 150 %.
