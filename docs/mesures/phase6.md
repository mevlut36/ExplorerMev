# Mesures de la phase 6 — distribution

Même banc que les phases précédentes (Ryzen 5 5600X, Windows 11 Pro, échelle 100 %). Cette phase
n'a pas de cible chiffrée : on vérifie ce que les livrables sont, ce qui a été contrôlé, et que
rien n'a régressé.

## Livrables (`packaging\package.ps1`)

| Livrable | Taille | Contrôlé |
|---|---|---|
| `explorermev.exe` (icône et version intégrées) | 15,1 Mo | informations de version lues (produit, description, 0.1.0) ; icône extraite de l'exécutable |
| `ExplorerMev_0.1.0_x64.msix` | 7,3 Mo | `makeappx pack` (schéma du manifeste validé) ; signature présente ; désarchivage : `explorermev.exe` identique octet pour octet à l'exécutable compilé ; identité (nom, version 0.1.0.0, x64, éditeur = sujet du certificat) |
| `ExplorerMev_0.1.0_x64_portable.zip` | 7,2 Mo | extrait dans un dossier vide : `explorermev.exe` identique à l'exécutable compilé, `--version` → « ExplorerMev 0.1.0 », `--win-e status` → « Explorateur » |
| `ExplorerMev.cer`, `SHA256SUMS.txt` | — | empreintes recalculées par le script |
| Manifestes winget (3 fichiers, schéma 1.6.0) | — | `winget validate` : **réussi** (avec une URL de documentation `example.com`, non publiée) |

## Signature

- Certificat **auto-signé** créé dans le magasin personnel de l'utilisateur (`CurrentUser\My`).
  Vérifié : il n'est présent dans **aucun** magasin de confiance (racines de confiance, éditeurs
  approuvés, personnes approuvées : utilisateur courant et machine).
- `Get-AuthenticodeSignature` : `UnknownError` ; `signtool verify /pa` : « A certificate chain
  processed, but terminated in a root certificate which is not trusted by the trust provider ».
  Résultat attendu pour un certificat auto-signé ; **ce n'est pas une signature approuvée**.
- Pas d'horodatage (aucun envoi réseau ; option `-TimestampUrl` pour un vrai certificat).

## Icône

Dessin original généré par `packaging\make-icons.ps1` : carré bleu arrondi, dossier jaune et
loupe (omise sous 48 px, où elle devient illisible). `app.ico` : 16, 24, 32, 48, 64, 128 et
256 px ; 6 images pour le manifeste MSIX. Vérifié par capture de **la fenêtre de l'application**
(barre de titre) : l'icône apparaît, ce qui a exigé d'envoyer `WM_SETICON` (winit ne le fait pas).

## Non-régression

| Cible | Résultat |
|---|---|
| Démarrage < 250 ms | 70,7–82,0 ms (première image, 5 passages) |
| Mémoire, dossier de 1 000 éléments | 16,3 Mo privés / 40,6 Mo WS |
| 100 000 fichiers : premières lignes / liste triée | peintes en 9,8–16,0 ms / 47,6–52,9 ms |
| Défilement, 100 000 éléments (2 passages) | 158–161 i/s, **0** image > 16,7 ms |
| Interface jamais bloquée | latence max 2,9 ms pendant l'auto-test, 0 mesure > 50 ms |
| Auto-test | **34 sur 34** |
| `cargo test --workspace` | 116 réussis, 3 ignorés ; clippy et rustfmt sans remarque |

## Non vérifié

- **Installation du MSIX** : n'a pas été faite par moi (elle exige d'approuver le certificat
  auto-signé, invite administrateur) mais par l'utilisateur, qui l'a installé : l'installation
  réussit et l'alias `explorermev` fonctionne dans un terminal ; en lançant
  `explorermev --default-explorer on`, il a obtenu le refus prévu pour un paquet qui isole le
  registre, ce qui prouve que la détection d'identité de paquet fonctionne en réel. Toujours non
  observés : les tuiles, l'emplacement réel des réglages dans le paquet, le lancement de
  l'assistant élevé depuis le paquet.
- **Installation par winget** : l'URL de l'archive n'existe pas ; `winget install` n'a pas été
  essayé (il faudrait aussi activer les manifestes locaux, réglage administrateur).
- **Smartscreen / antivirus** sur un exécutable non signé : non évalué.
- **Fichiers de licence** : aucune licence dans le dépôt ; le manifeste winget généré porte
  `Proprietary` par défaut, à remplacer.

## Option « explorateur par défaut » (ajoutée après la phase 6)

- Commande de la palette et `explorermev --default-explorer on|off|status` : Win+E et ouverture des
  dossiers et lecteurs (verbe `open` de `Directory` et `Drive`, clés du compte courant, avec
  marqueur). Remplace l'ancienne commande « Remplacer Win+E » (le réglage `--win-e` reste).
- **Testé** (4 tests, clé de test de `HKCU`, supprimée ensuite ; aucune clé de l'utilisateur
  modifiée) : activation, mise à jour du chemin, désactivation qui rétablit tout (y compris la
  valeur par défaut de `shell`), clé voisine d'un autre outil intacte, refus si un autre
  programme a un verbe `open` ou un verbe par défaut, état partiel signalé puis réparé.
- **Vérifié** : ExplorerMev lit correctement l'argument `"%1\."` pour un lecteur (`D:`), un chemin
  avec espaces et un dossier ordinaire (4 instances lancées puis fermées).
- **Vérifié en réel** (option activée environ une minute puis désactivée, sans frappe simulée) :
  trois ouvertures de dossier par l'API du Shell lancent ExplorerMev au lieu de l'Explorateur :
  `Start-Process C:\Windows\System32` (titre « System32 — ExplorerMev »), `start D:\` (« D:\ —
  ExplorerMev », ligne de commande `"D:\\."`) et `ShellExecute` sans verbe sur `C:\Users\Public`
  (équivalent de Win+R, « Public — ExplorerMev »). Le nombre de fenêtres de l'Explorateur n'a pas
  changé (1 avant, 1 après). Après désactivation : `--default-explorer status` → « Windows »,
  clés `Directory\shell\open`, `Drive\shell\open` et celle de Win+E absentes, valeur par
  défaut de `Directory\shell` revenue à vide. La machine est revenue à son état d'origine.
- **Non vérifié** : la touche Win+E elle-même (aurait demandé une frappe simulée, que je
  m'interdis ; la même commande est déjà testée sur une clé de test) ; l'option activée depuis la
  palette (la confirmation) plutôt que par `--default-explorer on` (même code d'écriture).

## « Explorateur par défaut » dans le paquet MSIX

Constaté par l'utilisateur : `explorermev --default-explorer on` depuis le paquet installé a été
refusé (« le registre y est isolé »). Réponse : au lieu d'interdire l'option dans le paquet, le
manifeste désactive la virtualisation d'écriture du registre (`unvirtualizedResources`), la
commande enregistrée désigne l'alias d'exécution, et l'application refuse seulement si le paquet
installé virtualise encore.

- **Vérifié** : le paquet se construit (schéma validé par `makeappx`), est signé par le
  certificat déjà approuvé, a une version (0.1.0.7770) supérieure à celle installée (0.1.0.0) ;
  son manifeste contient bien la désactivation et la capacité ; 121 tests réussis (lecture du
  drapeau du manifeste, chemin de lancement hors paquet).
- **Non vérifié** : l'effet réel dans le paquet mis à jour (que Windows ouvre ExplorerMev pour un
  dossier par l'alias). À observer après `Add-AppxPackage`.
