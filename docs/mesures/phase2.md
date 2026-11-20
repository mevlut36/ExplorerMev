# Mesures de la phase 2

Même banc de test qu'aux phases précédentes (Ryzen 5 5600X, Windows 11, 2560×1440 à 165 Hz),
profil `release`, moteur de rendu logiciel, cache chaud.

## Performances après ajout des icônes, de la surveillance et de l'hôte Shell

| Mesure | Cible | Phase 1 | Phase 2 |
|---|---|---|---|
| Démarrage, première image | < 250 ms | 67–86 ms | 81–92 ms |
| Mémoire au repos (profil utilisateur) | < 60 Mo | 12,1 Mo privés | 15,5 Mo privés / 41,5 Mo WS |
| Mémoire, dossier de 1 000 éléments | < 60 Mo | 12,8 Mo | 15,9 Mo privés / 43,6 Mo WS |
| 100 000 fichiers : premières lignes peintes | < 150 ms | 9,6–15,6 ms | 9,1–16,3 ms |
| 100 000 fichiers : liste triée peinte | < 1 s | 65,5–73,8 ms | 47,7–59,2 ms |
| Mémoire avec 100 000 éléments affichés | — | 36,7 Mo | 40,5 Mo privés |

Le démarrage gagne 10 à 15 ms : initialisation OLE du thread d'interface, démarrage de l'hôte
Shell, des deux threads d'icônes et du volet de navigation. Il reste très en dessous de la cible.

## Auto-test fonctionnel

`explorermev --self-test <dossier vide>` exécute un scénario dans l'application réelle, sans
aucune entrée souris ou clavier synthétique. Les étapes appellent les mêmes fonctions que les
callbacks de l'interface. Résultat : **11 vérifications sur 11** :

1. fichiers créés par un autre programme affichés par la surveillance ;
2. nouveau dossier créé (`IFileOperation`) et passé en renommage en place ;
3. nom proposé « Nouveau dossier » ;
4. renommage appliqué sur le disque ;
5. élément renommé sélectionné ;
6. Ctrl+A ;
7. tri par taille, dossiers en premier ;
8. saisie au clavier pour sauter à un élément ;
9. menu contextuel natif affiché (30 entrées) puis fermé automatiquement ;
10. suppression externe appliquée sans relister ;
11. sélection reportée sur l'élément suivant.

## Tests automatisés

`cargo test --workspace` : 78 tests réussis, 1 ignoré (presse-papiers, voir ci-dessous).

## Non vérifié en conditions réelles

- **Presse-papiers** : test écrit mais ignoré par défaut, car il remplace le contenu du
  presse-papiers de l'utilisateur
  (`cargo test -p fx-win --test shell -- --ignored clipboard`).
- **Glisser-déposer depuis et vers la fenêtre** : la cible est testée au niveau COM ; le geste
  complet à la souris n'a pas été exécuté (pas d'entrée synthétique sur le poste de
  l'utilisateur).
- **Ouverture d'un fichier** (double-clic, Entrée) et **suppression vers la corbeille** depuis
  l'interface : non exécutées, pour ne pas lancer d'applications ni remplir la corbeille.
  `IFileOperation` est testé en suppression définitive silencieuse.
- **Sélection rectangulaire, autocomplétion** : logique testée unitairement ; geste à la
  souris non exécuté.
