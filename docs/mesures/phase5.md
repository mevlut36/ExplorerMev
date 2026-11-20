# Mesures de la phase 5 — recherche instantanée

Même banc : Ryzen 5 5600X (6 cœurs / 12 threads), Windows 11 Pro, SSD NVMe (C:), échelle 100 %.
Profil `release`. Volumes indexés : C: (2,42 millions d'éléments), D: (22 241), E: (5 289),
F: (5). Les modes de mesure utilisent leur propre cache d'index (`index-mesure`).

## Cible

| Cible | Mesure | Résultat | Verdict |
|---|---|---|---|
| Recherche < 50 ms par frappe | `--bench-search` ×5 : 31 frappes à 120 ms d'intervalle (4 requêtes tapées lettre par lettre) | frappe → résultats affichés : p50 8,4–9,1 ms, p95 15,6–16,9 ms, **max 16,6–17,0 ms** ; 31 frappes sur 31 affichées | atteint |
| Idem, pire requête du moteur seul | `search_probe` (15 requêtes ×7) | « a b c » (3 mots d'une lettre, 645 000 résultats) : 43,1 ms au pire ; « e » (1,7 million de résultats) : 25,1 ms | atteint |
| Thread d'interface jamais bloqué | sonde de latence pendant la construction de l'index, la frappe et l'auto-test | max 3,9 à 9,7 ms, **0** mesure > 50 ms | atteint |

## Index

| | Résultat |
|---|---|
| Parcours de C: sans droits (comptage seul, `walk_probe`) | 56,1 s cache froid ; 9,3 s cache chaud |
| Construction de l'index de C:, pleine priorité (`search_probe`) | 9,7 s |
| Construction dans l'application, priorité arrière-plan, moitié des cœurs, sans cache | 28,6 s puis 13,4 s (latence max de l'interface 4,6–5,4 ms) |
| Taille en mémoire (index seul, C:) | 120 Mo |
| Cache disque (4 volumes) | 104,5 Mo ; écriture 1,4 s ; relecture vérifiée 60–65 ms (C:) |
| Index prêt à l'ouverture de la recherche, depuis le cache | 0,20 s (4 volumes) |
| Première frappe après libération de l'index (relu dès l'ouverture de la recherche) | 7 ms (sans relecture anticipée : 126–129 ms) |

## Mémoire du processus

| État | Privée | Working set |
|---|---|---|
| Au repos, avant toute recherche | 15,5–16,0 Mo | 39,5–39,8 Mo |
| Index chargé (depuis le cache) | 133,5–133,7 Mo | 159 Mo |
| Index chargé (juste construit) | 146,5–148,2 Mo | 164–165 Mo |
| Après 5 min sans recherche (index libéré) | 22,7–28,8 Mo | 48–52 Mo |

La cible « < 60 Mo au repos » reste atteinte : l'index n'est en mémoire que pendant et peu
après une recherche.

## Corrections faites pendant la mesure

| Problème mesuré | Correction |
|---|---|
| La première insertion après un chargement doublait les tableaux de l'index : +110 Mo (133 → 243 Mo) pour un seul fichier créé | croissance par 1/16 au lieu du doublement |
| Index libéré, mais mesure à 243 Mo : l'état « libéré » relançait la requête affichée, qui relisait l'index | pas de relance pendant qu'une requête attend ; banc corrigé |
| Première frappe après libération : 126–129 ms (relecture du cache) | relecture dès l'ouverture de la recherche : 7 ms |
| Renommer un dossier retirait aussi ses descendants | l'ancien nom d'un renommage ne retire que l'élément |
| Fichier créé dans un dossier apparu pendant que l'application était fermée : introuvable | les dossiers manquants sont ajoutés en remontant les parents |
| Fichier rangé sous un **fichier** sans rapport : NTFS avait réutilisé le numéro d'enregistrement d'un élément supprimé, encore présent dans un cache périmé | un numéro déjà indexé est vérifié (nom, parent, dossier) avant d'être réutilisé |
| Assistant MFT : noms des dossiers non listables transmis à l'interface (écart avec l'ADR 0003) | filtre d'accès dans l'assistant, sur tout le chemin |

## Auto-test (`--self-test`)

**34 vérifications sur 34, trois passages de suite.** Aux 30 de la phase 4 s'ajoutent : index
prêt, fichier créé pendant que l'application tourne trouvé par la recherche, Entrée qui ouvre
son dossier et le sélectionne, fichier supprimé retiré de l'index. Les étapes qui attendaient un
délai fixe attendent maintenant leur condition (5 s au plus) : un délai fixe échouait
occasionnellement sur la machine chargée (renommage du nouveau dossier pas encore commencé).

## Non-régression

| Cible | Résultat |
|---|---|
| Démarrage | 67–80 ms (première image) |
| Mémoire, dossier de 1 000 éléments | 16,7 Mo privés |
| 100 000 fichiers : premières lignes / liste triée | peintes en 9,1–15,6 ms / 46,5–52,9 ms |
| Défilement, 100 000 éléments | 3 passages : 0, 1 et 3 images > 16,7 ms, isolées (pointes de 20 et 42 ms à CPU 30 %, même signature que les perturbations extérieures de la phase 4) ; 159–160 i/s |

## Tests

`cargo test --workspace` : 115 réussis, 3 ignorés (presse-papiers, OneDrive, délai SMB). Dont :
index (recherche, classement, renommages, suppressions, cache abîmé refusé), protocole de
l'assistant (trames malformées refusées), tube (ACL, première instance), contrôle d'accès
(dossier refusé par la DACL), filtre de l'assistant (ancêtre refusé, boucle, déplacement),
assistant réel lancé sans élévation (trame d'erreur, arrêt), mise à jour en direct sur disque.

## Non mesuré

- **Mode MFT** (assistant élevé) : je n'ai pas lancé l'invite d'élévation sur la machine de
  l'utilisateur sans son accord. Durée d'énumération, mémoire de l'assistant et coût du filtre
  d'accès restent à mesurer.
- Recherche sur un disque dur mécanique, sur un volume de plusieurs dizaines de millions de
  fichiers.
