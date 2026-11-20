# Notes Win32 / Slint / winit

Constats sur des comportements mal documentés ou surprenants. Chaque note indique comment le
reproduire.

## Mica : `DwmExtendFrameIntoClientArea` n'est pas nécessaire avec winit

- **Constat** : `DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW)` suffit à
  afficher Mica sous les zones transparentes d'une fenêtre Slint, avec les moteurs logiciel,
  femtovg et skia. Appeler en plus `DwmExtendFrameIntoClientArea` avec des marges à -1 ne
  change rien de visible.
- **Explication probable** : winit crée la fenêtre avec `with_transparent(true)` (c'est le
  réglage par défaut de Slint sous Windows ; voir `winitwindowadapter.rs` dans
  i-slint-backend-winit 1.19) et rend ainsi la surface composée avec alpha. Le fond de fenêtre
  Slint doit valoir `transparent`.
- **Reproduction** : `PROTO_NO_EXTEND_FRAME=1 slint-scroll --renderer software`, puis comparer
  la capture avec celle obtenue sans la variable.
- **Conséquence** : `fx-win` n'appelle que `DwmSetWindowAttribute`. La barre de titre suit le
  thème via `DWMWA_USE_IMMERSIVE_DARK_MODE`.

## Moteur logiciel Slint : pas de rendering notifier

- **Constat** : `Window::set_rendering_notifier` renvoie `SetRenderingNotifierError::Unsupported`
  avec le moteur `software`.
- **Contournement** : horodater `WindowEvent::RedrawRequested` via
  `WinitWindowAccessor::on_winit_window_event`. On obtient le début de chaque image ; pour la fin
  du rendu, un `Timer::single_shot(Duration::ZERO)` armé à ce moment donne une borne haute.

## Moteur skia (Direct3D) : notifier accepté mais jamais appelé

- **Constat** : `set_rendering_notifier` réussit, mais aucun `BeforeRendering` ni
  `AfterRendering` n'est reçu pendant 12 s de défilement. Point non approfondi, le moteur étant
  écarté pour sa mémoire (226 Mo privés).

## `slint::Window::set_maximized` ignoré au démarrage

- **Constat** : `set_maximized(true)`, appelé avant ou juste après `show()`, reste sans effet
  (la fenêtre garde 1100×720). Avec winit, la fenêtre native n'est créée qu'au démarrage de la
  boucle d'événements.
- **Contournement** : appeler `winit::window::Window::set_maximized(true)` au premier événement
  de fenêtre (`on_winit_window_event`). Pour `fx-app` (restauration de session) : appliquer
  l'état de fenêtre à cet endroit.

## Pilotes GPU : coût mémoire dans le processus

- **Constat** : le pilote OpenGL AMD (Radeon RX 6600 XT, 32.0.21045.29003) ajoute environ
  160 Mo de mémoire privée et environ 230 ms au démarrage d'un processus femtovg. Skia (Direct3D)
  est encore plus lourd (226 Mo).
- **Conséquence** : ADR 0001, moteur logiciel par défaut.

## Fenêtre auxiliaire WGL

- **Constat** : avec femtovg, `Process.MainWindowHandle` (PowerShell/.NET) renvoie au début une
  fenêtre de 16×16 px créée pour charger les extensions WGL, et non la fenêtre principale.
- **Conséquence** : les outils de test qui cherchent la fenêtre doivent filtrer par titre.

## PowerShell 5.1 et sortie d'erreur des programmes natifs

- **Constat** : avec `$ErrorActionPreference = "Stop"`, la simple progression de cargo sur
  stderr (« Compiling… ») interrompt le script.
- **Conséquence** : les scripts du dépôt testent `$LASTEXITCODE` au lieu de passer par
  `ErrorActionPreference`.
