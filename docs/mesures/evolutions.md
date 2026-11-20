# Évolutions d'interface après la phase 6

Mêmes conditions de mesure que les phases précédentes (Ryzen 5 5600X, Windows 11 26H2 build 26300,
échelle 100 %, profil `release`, rendu logiciel). Les modes de mesure ne modifient jamais les
données de l'utilisateur (vérifié : favoris, apparence et session identiques avant et après).

## Ce qui a été ajouté

| Demande | Réalisation |
|---|---|
| Win+E n'ouvre pas ExplorerMev | La commande du Registre n'est pas consultée par Windows 11 26H2 : agent résident d'interception clavier (voir « Non vérifié ») |
| Côté droit vide, lignes qui s'arrêtent à « Taille » | Le second volet caché réservait 160 px ; la colonne Nom s'étire ; survol, sélection et clic droit couvrent toute la ligne |
| Champ d'adresse trop court | Prend toute la largeur de la barre |
| Barre d'outils sous la barre de chemin | Nouveau dossier / fichier, couper, copier, coller, renommer, supprimer, propriétés ; libellés selon la largeur ; boutons activés selon la sélection et le presse-papiers |
| Mise en évidence animée des éléments créés | Fondu d'apparition puis de disparition (~1,6 s) pour les éléments créés ou collés |
| Changer les couleurs | Menu Apparence : mode système/clair/sombre, 8 couleurs d'accent, fond teinté ; mémorisé ; barre de titre et Mica suivent le mode |
| Boutons de souris précédent / suivant | `MouseButton::Back` et `Forward` |
| `\\wsl.localhost` | Racine « Linux » virtuelle, distributions dans le volet de gauche, fil d'Ariane, dossier parent, titres |
| Page d'accueil | Accès rapide, épinglés (aussi dans le volet de gauche), récents ; page de départ par défaut |

## Mesures de non-régression

| Cible | Mesure | Résultat |
|---|---|---|
| Démarrage < 250 ms | `--bench-startup` ×5 | 75,9–97,2 ms (première image) |
| Mémoire < 60 Mo (1 000 éléments) | `--bench-open n1000` | 16,6 Mo privés / 41,1 Mo WS |
| 100 000 fichiers : premier affichage / liste triée | `--bench-open n100000` | peintes en 9,5–15,7 ms / 52,2–54,4 ms |
| Défilement à 60 i/s, détails | fenêtre, 2 passages | 160,3–161,1 i/s, **0** image > 16,7 ms |
| Idem, plein écran | 2 passages | molette et rapide 159 i/s, sauts 139 i/s, **0** image > 16,7 ms |
| Idem, vue icônes | fenêtre, 2 passages | 159,9–161,8 i/s, **0** image > 16,7 ms |
| Interface jamais bloquée | sonde de latence, tous les modes | max 3,3 à 16,8 ms, **0** mesure > 50 ms |

Le démarrage des deux premiers passages (97,2 et 95,5 ms) est plus lent que les trois suivants
(75,9–82,6 ms) : variation de cache du système, sans dépassement de la cible.

## Vérifié dans l'application

Auto-test (`--self-test`) : **52 vérifications sur 52, deux passages de suite**. Aux 34 de la phase 5
s'ajoutent : état des boutons de la barre d'outils, création d'un dossier et d'un fichier par la barre
d'outils (sur le disque), mise en évidence allumée puis éteinte, WSL (racine Linux, ouverture d'une
distribution, dossier parent), apparence appliquée (clair + rose + teinté, puis sombre), accueil
(affichage, lignes, épinglage et retrait, précédent, suivant). Captures de la fenêtre de
l'application : mises en page, thèmes clair rose, sombre vert et menu Apparence, page d'accueil.

`cargo test --workspace` : 136 réussis, 3 ignorés ; clippy et rustfmt sans remarque.

## Non vérifié

- **Win+E (agent de raccourci)** : l'exécution de l'agent m'a été refusée par le système de permissions
  (crochet clavier global) et je ne simule aucune frappe. Vérifiés par des tests : décision de capture
  (Win+E seul, ni injecté ni combiné), état de l'option, valeur de démarrage automatique dans une clé de
  test. Non observés : le démarrage de l'agent, l'instance unique, l'arrêt, l'interception d'une vraie
  frappe, l'absence d'ouverture du menu Démarrer.
- **Boutons latéraux de la souris** : le code reçoit les événements de winit ; aucun bouton physique
  n'a pu être pressé pendant mes essais.
- **Menu Apparence au clic** : l'effet des choix est vérifié (callback invoqué par l'auto-test et
  captures), pas le clic de souris lui-même.
- **Fichiers épinglés dans le volet de gauche** : le code ouvre un favori qui est un fichier ; seule la
  partie dossiers est exercée par l'auto-test.
- **Fond teinté et thème clair** : lisibilité jugée sur capture ; pas de mesure de contraste.
