# ADR 0003 — Stratégie d'index pour la recherche instantanée

- **Statut** : accepté, implémenté en phase 5 (2026-11-20) ; remplace la proposition de la
  phase 0 (voir « Écarts avec la proposition initiale »)
- **Date** : 2026-11-20

## Contexte

La recherche par nom doit répondre en moins de 50 ms à chaque frappe, sur tous les volumes
locaux (2,4 millions d'éléments sur le volume système de la machine de développement). Deux
sources sont possibles :

- la **MFT** (`FSCTL_ENUM_USN_DATA`) et le **journal USN** (`FSCTL_READ_USN_JOURNAL`) : complets,
  rapides, tenus à jour sans parcours, mais il faut ouvrir le **volume**, donc les droits
  d'administration (vérifié : « Accès refusé » sans élévation) ;
- un **parcours des dossiers** sans droits : 9 à 56 s pour le volume système (cache chaud ou
  froid), puis une surveillance récursive (`ReadDirectoryChangesW` sur la racine).

L'application reste `asInvoker` (pas d'élévation globale : elle casserait le glisser-déposer
avec les processus non élevés).

## Décision

1. **Index dans le processus de l'interface**, sur un thread dédié (`fx-search`) :
   - éléments désignés par leur **numéro d'enregistrement MFT**, quelle que soit la source
     (`FileIdExtdDirectoryInfo` donne la même référence lors du parcours) ; parent désigné de
     même ; 16 octets par élément, noms UTF-8 contigus, table « enregistrement → élément » ;
   - recherche : `memchr` sur le premier caractère (minuscule et majuscule) dans le tableau des
     noms, vérification insensible à la casse, en parallèle par tranches, annulable ; classement
     nom exact > début du nom > reste, noms courts d'abord ; chemins construits pour les
     1 000 premiers seulement ;
   - **libéré de la mémoire après 5 minutes sans recherche** (cache écrit d'abord s'il a
     changé), **relu dès l'ouverture** de la recherche ; les changements reçus entre-temps sont
     notés et rejoués.
2. **Cache disque** par volume (`%LOCALAPPDATA%\ExplorerMev\index\X.idx`), vérifié borne par borne à
   la lecture (un fichier abîmé est ignoré) et rattaché au numéro de série du volume.
3. **Source par défaut : parcours sans droits**, en priorité « arrière-plan » (processeur et
   disque) avec la moitié des cœurs, relancé si le cache a plus de 10 minutes. Mise à jour par
   surveillance récursive de chaque volume.
4. **Source rapide, sur demande : assistant élevé** `explorermev --index-helper`, lancé par une
   commande confirmée de la palette (ou le réglage `index_mft = true`) :
   - **sens unique** : l'interface crée le tube en **lecture seule pour elle**
     (`PIPE_ACCESS_INBOUND`) ; l'assistant ne peut qu'écrire, il ne reçoit aucune commande ;
   - tube à nom aléatoire (128 bits), `FILE_FLAG_FIRST_PIPE_INSTANCE`,
     `PIPE_REJECT_REMOTE_CLIENTS`, DACL `D:P(A;;GA;;;<SID de l'utilisateur>)`, et vérification
     que le client est bien le processus lancé (`GetNamedPipeClientProcessId`) ;
   - arguments de l'assistant vérifiés avant toute action (forme du nom de tube, pid, lettres) ;
   - trames bornées (4 Mio), lues défensivement par l'interface ;
   - **filtre d'accès dans l'assistant** : un nom n'est transmis que si **tous** les dossiers de
     son chemin sont listables avec le jeton **non élevé** de l'utilisateur (jeton lié,
     `AccessCheck` du droit `FILE_LIST_DIRECTORY`, une fois par dossier et par descripteur de
     sécurité ; refus dans le doute). L'index ne contient donc rien de plus que ce qu'un
     parcours sans droits verrait ;
   - l'assistant s'arrête quand l'interface se ferme ou que le tube se rompt.

## Écarts avec la proposition initiale

| Proposition (phase 0) | Décision | Raison |
|---|---|---|
| Service Windows `fx-indexd` installé, index dans le service | Assistant élevé lancé à la demande, index dans l'interface | Pas d'installation ni de service permanent avant la phase 6 ; l'index est libéré quand on ne cherche pas (mémoire au repos inchangée) |
| Requêtes envoyées au service (protocole requête/réponse) | Flux à sens unique de l'assistant vers l'interface | Surface d'attaque minimale du processus élevé : il ne lit rien |
| Filtre d'accès au moment de la requête (emprunt d'identité) | Filtre à la source, dans l'assistant, avant envoi | Aucun nom interdit ne quitte le processus élevé |
| Repli par Windows Search | Repli par parcours complet + surveillance | Couverture totale et résultats identiques dans les deux modes |

## Limites connues

- **Liens physiques** : un enregistrement n'est indexé que sous un de ses noms.
- **Droits resserrés après coup** : si un dossier devient non listable, les noms déjà transmis
  restent dans l'index jusqu'à sa prochaine construction.
- **Changements pendant la fermeture de l'application** (mode parcours) : absents jusqu'à la
  reconstruction en arrière-plan (cache de plus de 10 minutes).
- **Mode MFT non mesuré** sur la machine de développement (élévation non autorisée pendant le
  développement) ; testé sans élévation de bout en bout (tube, contrôle du pid, protocole,
  trame d'erreur), et la logique du filtre est testée avec un contrôle simulé.
- ReFS, FAT/exFAT : non indexés (volumes NTFS fixes seulement).

## Alternatives écartées

- **Élévation de l'application entière** : contraire à `asInvoker`, casse le glisser-déposer
  avec les processus non élevés (UIPI).
- **Lecture brute de la MFT sans journal USN** : pas de mise à jour incrémentale.
- **Windows Search seul** : couverture partielle, latence supérieure à la cible.
