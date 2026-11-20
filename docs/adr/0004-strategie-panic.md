# ADR 0004 — Stratégie de panic : `unwind` en release

- **Statut** : accepté (phase 1, 2026-11-20)

## Contexte

Le cahier des charges demande deux choses incompatibles :

1. `panic = "abort"` pour le binaire ;
2. « un panic dans un worker ne doit jamais faire tomber l'application ni corrompre l'état ;
   il est journalisé et la requête échoue proprement ».

Avec `abort`, `std::panic::catch_unwind` ne rattrape rien : le moindre panic dans un thread de
listing tue tout le processus. La seconde exigence est donc impossible à tenir.

## Mesure

Même code, profil release (LTO fat, codegen-units 1, strip), seule la stratégie change :

| | Taille de `explorermev.exe` | Démarrage (1ʳᵉ image, 5 passages) |
|---|---|---|
| `abort` | 11,15 Mo | 64–91 ms |
| `unwind` | 13,45 Mo (+2,3 Mo) | 64–86 ms |

Pas de différence mesurable au démarrage (variance du système supérieure à l'écart). Le coût
porte sur la taille du binaire uniquement.

## Décision

`panic = "unwind"` dans le profil release.

- Les threads de travail exécutent leurs tâches sous `catch_unwind` et transforment un panic en
  échec propre de la requête (`LoadEvent::Failed`). Le test
  `loader::tests::worker_panic_becomes_a_clean_failure` le vérifie.
- Un hook de panic journalise le message et le nom du thread (`fx-app`).
- Les workers ne partagent pas d'état mutable avec l'interface (messages seulement) : un panic
  ne peut pas laisser de verrou empoisonné visible par l'interface.

## Conséquences

- +2,3 Mo sur le binaire. Si la taille devient un enjeu (distribution, phase 6), on pourra
  réévaluer, mais seulement si on renonce explicitement à l'exigence d'isolation.
- Un panic sur le **thread d'interface** reste fatal : la boucle Slint n'est pas conçue pour
  continuer après. Le code de l'interface ne doit donc pas paniquer (`unwrap`/`expect`
  interdits par les lints hors tests).
