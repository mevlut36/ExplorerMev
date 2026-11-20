# Mesures de la phase 1

Même banc de test qu'en phase 0 : Ryzen 5 5600X, 32 Go, Radeon RX 6600 XT, Windows 11 Pro
10.0.26300, écran 2560×1440 à 165 Hz, échelle 100 %. Rust 1.100.0, profil `release`
(LTO fat, `panic = "unwind"`). Moteur de rendu logiciel. **Cache chaud** dans tous les cas.

## Reproduction

```powershell
cargo run --release -p fx-bench --bin gen-fixtures        # jeux de 1 000 / 10 000 / 100 000 entrées
cargo bench -p fx-bench                                   # listing, clés de tri, tri, filtre
cargo build --release
target\release\explorermev.exe --bench-startup
target\release\explorermev.exe --bench-open target\fixtures\n100000 --iterations 5
target\release\explorermev.exe --bench-open target\fixtures\n1000 --iterations 3
```

Dans l'application, les temps d'ouverture partent de la demande de navigation. « Peint » signifie
que le rendu de l'image contenant ces lignes est terminé (borne haute : timer à délai nul après
`RedrawRequested`).

## Application : démarrage et ouverture de dossier

| Mesure | Cible | Résultat | Verdict |
|---|---|---|---|
| Démarrage jusqu'à la première image (profil utilisateur, 72 éléments, affiché dans cette image) | < 250 ms | 67–86 ms (5 passages) | atteint (à chaud) |
| Mémoire au repos, dossier de 1 000 éléments | < 60 Mo | 12,8 Mo privés / 33,9 Mo WS | atteint |
| 100 000 fichiers : premières lignes peintes | < 150 ms | 9,6–15,6 ms | atteint |
| 100 000 fichiers : liste complète triée peinte | < 1 s | 65,5–73,8 ms | atteint |
| Mémoire avec 100 000 éléments affichés | — | 36,7 Mo privés / 52,1 Mo WS | — |
| Défilement à 60 i/s | 60 i/s | **non mesuré dans l'application** (prototype de la phase 0 seulement) | à faire en phase 3 |
| Thread d'interface jamais bloqué par une E/S | — | par construction (listing, tri, filtre et Shell hors du thread d'interface), non instrumenté | à instrumenter en phase 3 |

Détail d'une ouverture de 100 000 fichiers (5 itérations) : listing 53,6–61,2 ms, clés et tri
5,7–6,2 ms.

## Briques (criterion, médianes)

| | 1 000 | 10 000 | 100 000 |
|---|---|---|---|
| Listing, méthode retenue (`FileIdExtdDirectoryInfo` 64 Kio) | 0,50 ms | 4,5 ms | 39,4 ms |
| Chargement synchrone complet (listing, types, clés, tri) | 1,06 ms | 7,8 ms | 72,7 ms |
| Clés de tri naturel (`LCMapStringEx`), parallèles au-delà de 20 000 | 0,18 ms | 1,8 ms | 3,9 ms |
| Tri par nom avec clés (parallèle au-delà de 20 000) | 0,11 ms | 1,7 ms | 2,0 ms |
| Filtre « rapport 4 » | 0,35 ms | 3,4 ms | 6,4 ms |

Comparatif complet des méthodes de listing : [ADR 0002](../adr/0002-methode-listing.md).

## Binaire

`explorermev.exe` : 13,45 Mo (moteurs logiciel et femtovg inclus, `panic = "unwind"`, voir
[ADR 0004](../adr/0004-strategie-panic.md)).

## Limites

- Cache chaud seulement ; démarrage à froid et ouverture à froid à mesurer en phase 3.
- Une seule machine ; les fichiers de test sont vides (la taille n'influe pas sur le listing).
- Le défilement de l'application elle-même n'a pas encore son banc de mesure (le prototype de
  la phase 0 l'a validé avec des lignes de même structure).
