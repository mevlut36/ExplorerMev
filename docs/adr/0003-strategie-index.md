# ADR 0003 — Stratégie d'index pour la recherche instantanée

- **Statut** : proposé (implémentation en phase 5)
- **Date** : 2026-11-20

## Contexte

La recherche par nom sur un volume NTFS doit répondre en moins de 50 ms à chaque frappe. Un
parcours du disque est bien trop lent. Lire la table MFT (`FSCTL_ENUM_USN_DATA`) puis suivre le
journal USN (`FSCTL_READ_USN_JOURNAL`) donne un index complet en quelques secondes et le tient
à jour en continu. Mais ces appels exigent un handle de **volume**, donc des droits
administrateur.

## Décision proposée

1. **Processus d'indexation séparé `fx-indexd`**, installé en option comme service Windows
   (installation élevée une seule fois). Il lit la MFT et le journal USN des volumes NTFS
   locaux et garde l'index en mémoire.
2. **Communication par named pipe** `\\.\pipe\fx-index-<SID>` :
   - DACL limitée au SID de l'utilisateur qui a installé le service, plus SYSTEM ;
     `FILE_FLAG_FIRST_PIPE_INSTANCE` pour empêcher un autre processus de squatter le nom ;
   - protocole minimal en **lecture seule** : `Query { motif, options, limite }` → résultats,
     `Status` → état de l'index, et aucune commande d'écriture ;
   - messages bornés en taille, désérialisation défensive.
3. **Contrôle d'accès des résultats** : le service voit des noms que l'utilisateur ne peut pas
   lister (profils d'autres comptes, dossiers protégés). Il emprunte l'identité du client
   (`ImpersonateNamedPipeClient`) et filtre chaque résultat par un contrôle d'accès en liste
   sur le dossier parent, mis en cache par dossier. Sans ce filtre, le service divulguerait des
   noms de fichiers.
4. **Structure en mémoire** : identifiants de fichier NTFS (FRN) remappés vers des indices
   denses `u32`, avec par entrée le parent (`u32`), un décalage dans une arène de noms (UTF-8
   pour le balayage rapide, avec un drapeau pour les noms non représentables), et les
   attributs. Budget visé : environ 16 octets par entrée plus les noms, soit autour de 70 Mo
   pour 2 millions de fichiers, **dans le service et non dans l'interface**.
5. **Requête** : balayage parallèle (`rayon`) de l'arène de noms avec `memchr::memmem` et repli
   insensible à la casse. Seuls les N premiers résultats affichés voient leur chemin
   reconstruit. Estimation : 40 Mo de noms balayés en quelques millisecondes sur 6 cœurs, à
   mesurer.
6. **Persistance** : instantané compact sur disque (en-tête avec l'identifiant du journal USN et
   le prochain USN). Au démarrage, on recharge l'instantané puis on rejoue le journal depuis ce
   USN. Si l'identifiant a changé ou si le USN a été purgé du journal, on réénumère la MFT.
7. **Repli sans élévation** (service absent) :
   - Windows Search (`ISearchQueryHelper` / OLE DB) pour les emplacements indexés par Windows ;
   - sinon, recherche par parcours parallèle limité au dossier courant et à ses sous-dossiers,
     avec la méthode de listing de l'ADR 0002, affichée en continu et annulable.
   - L'interface indique clairement le mode utilisé.

## Points à vérifier avant l'implémentation (phase 5)

- Prise en charge de `FSCTL_ENUM_USN_DATA` sur ReFS (le journal USN y existe ; l'énumération
  reste à tester).
- Coût réel du contrôle d'accès par dossier sur 10 000 résultats.
- Comportement à l'ajout ou au retrait d'un volume (clé USB, VHD) et à la mise en veille.

## Alternatives écartées

- **Élévation de l'application entière** : contraire à `asInvoker`, et elle casserait le
  glisser-déposer avec les processus non élevés (UIPI).
- **Lecture brute de la MFT sans journal USN** : pas de mise à jour incrémentale.
- **Windows Search seul** : couverture partielle (emplacements indexés seulement) et latence
  supérieure à la cible.
