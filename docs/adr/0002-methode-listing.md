# ADR 0002 — Méthode de listing des dossiers

- **Statut** : accepté (phase 1, 2026-11-20)
- **Mesures** : `cargo bench -p fx-bench --bench listing`, résultats dans
  [docs/mesures/phase1.md](../mesures/phase1.md)

## Contexte

Ouvrir un dossier de 100 000 fichiers doit afficher un premier écran en moins de 150 ms et la
liste complète triée en moins de 1 s, sans jamais bloquer le thread d'interface. Il faut aussi
obtenir, sans ouvrir chaque fichier : la taille, les dates, les attributs (dont les états
OneDrive) et le type de point d'analyse (lien symbolique, jonction, fichier cloud).

## Mesures (NTFS local, cache chaud, médiane criterion)

| Méthode | 1 000 | 10 000 | 100 000 |
|---|---|---|---|
| **A — `FileIdExtdDirectoryInfo`, tampon 64 Kio** | 0,50 ms | 4,5 ms | **39,4 ms** |
| A — tampon 256 Kio | 0,51 ms | 4,7 ms | 40,7 ms |
| A — tampon 1 Mio | 0,66 ms | 4,8 ms | 42,8 ms |
| B — `FileFullDirectoryInfo`, 64 Kio | 0,44 ms | 4,6 ms | 43,3 ms |
| C — `FindFirstFileExW` + `LARGE_FETCH` | 0,49 ms | 4,7 ms | 46,6 ms |
| D — `std::fs::read_dir` (+ `metadata()`) | 0,73 ms | 6,4 ms | 63,3 ms |

Les méthodes natives sont proches : le coût est surtout dans le noyau. Un tampon plus grand
n'apporte rien en local (il reste utile en réseau, pour réduire les allers-retours SMB : à
mesurer en phase 3).

## Décision

**A avec un tampon de 64 Kio, repli automatique sur B puis C** quand le système de fichiers
refuse la classe d'information (`ERROR_INVALID_PARAMETER`, `ERROR_NOT_SUPPORTED`,
`ERROR_INVALID_LEVEL`, `ERROR_INVALID_FUNCTION` au premier appel). Implémentation :
`fx_win::listing::list_dir`.

A est la plus rapide sur 100 000 entrées, et la seule qui donne directement
`ReparsePointTag`. B le fournit aussi, via `EaSize` pour les points d'analyse (vérifié par
test, voir NOTES-WIN32.md). Les trois méthodes renvoient des résultats identiques sur les
arborescences de test (`crates/fx-win/tests/listing.rs`).

## Conséquences

- Le handle de dossier est ouvert avec `FILE_LIST_DIRECTORY | SYNCHRONIZE`,
  `FILE_FLAG_BACKUP_SEMANTICS`, partage complet, via un chemin `\\?\` ; on vérifie
  ensuite qu'il s'agit bien d'un dossier (`FileBasicInfo`), car `BACKUP_SEMANTICS` ouvre aussi
  les fichiers.
- Les noms sont empruntés directement dans le tampon (pas de copie par entrée) et conservés en
  UTF-16 brut, substituts isolés compris.
- L'énumération ne lit jamais les données des fichiers : pas de téléchargement OneDrive. Lister
  un **dossier** cloud non peuplé peut interroger le service cloud ; c'est pourquoi le listing
  est annulable (consulté entre deux tampons). Le délai d'expiration réseau est prévu en phase 3.
- Le chargeur complet (listing, types, clés et tri) prend 54 à 61 ms dans l'application pour
  100 000 entrées : la cible de 1 s est tenue avec une large marge.
