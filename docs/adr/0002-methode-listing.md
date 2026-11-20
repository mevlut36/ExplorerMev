# ADR 0002 — Méthode de listing des dossiers

- **Statut** : proposé. La décision finale sera prise sur benchmark en phase 1.
- **Date** : 2026-11-20

## Contexte

Ouvrir un dossier de 100 000 fichiers doit afficher un premier écran en moins de 150 ms et la
liste complète triée en moins de 1 s, sans jamais bloquer le thread d'interface. Il faut aussi
obtenir, sans ouvrir chaque fichier : la taille, les dates, les attributs (dont les états
OneDrive) et le type de point d'analyse (lien symbolique, jonction, fichier cloud).

## Candidats

| # | Méthode | Atouts | Points d'attention |
|---|---|---|---|
| A | `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)` sur un handle de dossier, grand tampon | Lots de centaines d'entrées par appel ; fournit `ReparsePointTag` et l'identifiant de fichier 128 bits sans ouvrir les fichiers | Classe d'information refusée par certains systèmes de fichiers (FAT/exFAT, certains serveurs SMB) : repli obligatoire |
| B | `GetFileInformationByHandleEx(FileFullDirectoryInfo)` | Même principe, plus largement pris en charge | Pas de `ReparsePointTag` (seulement l'attribut `REPARSE_POINT`) |
| C | `FindFirstFileExW(FindExInfoBasic, FIND_FIRST_EX_LARGE_FETCH)` | Universel, robuste, nom court omis | Copie de chaque entrée dans un `WIN32_FIND_DATAW` (≈ 600 octets) ; tag de reparse dans `dwReserved0` |
| D | `std::fs::read_dir` | Aucun `unsafe` | Référence de comparaison uniquement ; implémentation interne à vérifier |

## Protocole de décision (phase 1)

- Bench `criterion` dans `benches/listing.rs` sur 1 000, 10 000 et 100 000 fichiers générés
  (script `scripts/gen-fixtures`), NTFS local, cache chaud et cache froid (volume de test
  démonté puis remonté, ou fichier d'échange vidé avec RAMMap si disponible).
- Pour A et B : tampons de 64 Kio, 256 Kio et 1 Mio.
- Mesures : temps jusqu'au premier lot de 1 000 entrées, temps total, allocations.
- Règle : on retient la méthode la plus rapide au temps total sur 100 000 entrées, à moins
  qu'une autre soit plus rapide de plus de 20 % au premier lot. Le repli suit l'ordre
  A → B → C, choisi par volume selon l'erreur `ERROR_INVALID_PARAMETER` / `ERROR_NOT_SUPPORTED`
  et mémorisé.

## Hypothèse de travail

A avec un tampon de 64 à 256 Kio, avec repli B puis C. A est la seule méthode qui donne
directement le tag de point d'analyse, nécessaire pour reconnaître les fichiers cloud et les
liens sans ouvrir de fichier.

## Contraintes indépendantes de la méthode

- Le handle de dossier est ouvert avec `FILE_LIST_DIRECTORY | SYNCHRONIZE`,
  `FILE_FLAG_BACKUP_SEMANTICS` et le partage complet (lecture, écriture, suppression), via un
  chemin `\\?\`.
- L'énumération ne lit jamais les données des fichiers, donc ne déclenche pas le téléchargement
  des fichiers OneDrive. En revanche, lister un **dossier** cloud non encore peuplé peut faire
  interroger le service cloud par le pilote de filtre : l'opération doit être annulable et
  soumise à un délai, comme un accès réseau.
- Les noms sont conservés en UTF-16 brut (y compris les paires de substitution invalides) et
  convertis pour l'affichage seulement.
- Les entrées sont émises par lots vers le thread d'interface (voir ARCHITECTURE.md).
