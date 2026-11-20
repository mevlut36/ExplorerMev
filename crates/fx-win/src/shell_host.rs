//! Hôte Shell : thread STA initialisé par OLE, avec boucle de messages et fenêtre cachée.
//!
//! Il exécute les tâches Shell qui exigent un appartement STA vivant avec pompe de messages
//! (menu contextuel, presse-papiers OLE, dialogues lancés par les verbes Shell), sans jamais
//! bloquer le thread d'interface. La fenêtre cachée sert de propriétaire aux menus et relaie
//! `WM_INITMENUPOPUP`, `WM_DRAWITEM`… à `IContextMenu3` pour que les sous-menus dynamiques
//! (« Envoyer vers », « Ouvrir avec ») et les icônes des extensions s'affichent.

use std::cell::RefCell;
use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleFlushClipboard, OleInitialize, OleUninitialize};
use windows::Win32::UI::Shell::{IContextMenu, IContextMenu2, IContextMenu3};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EndMenu, GetMessageW, KillTimer, MSG,
    PostMessageW, PostQuitMessage, RegisterClassExW, TranslateMessage, WM_APP, WM_CLOSE, WM_DESTROY,
    WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR, WM_TIMER, WNDCLASSEXW, WS_EX_TOOLWINDOW,
    WS_POPUP,
};
use windows::core::{Interface, w};

const WM_RUN_JOBS: u32 = WM_APP + 1;
pub(crate) const TIMER_DISMISS_MENU: usize = 0xF1;

/// Contexte passé aux tâches exécutées sur l'hôte.
pub struct HostCtx {
    /// Fenêtre cachée de l'hôte (propriétaire des menus).
    pub(crate) hwnd: HWND,
}

type Job = Box<dyn FnOnce(&HostCtx) + Send>;

thread_local! {
    static JOBS: RefCell<Option<Receiver<Job>>> = const { RefCell::new(None) };
    static ACTIVE_MENU: RefCell<Option<IContextMenu>> = const { RefCell::new(None) };
}

/// Déclare le menu contextuel affiché, pour le relais des messages de menu.
pub(crate) fn set_active_menu(menu: Option<IContextMenu>) {
    ACTIVE_MENU.with_borrow_mut(|m| *m = menu);
}

pub struct ShellHost {
    hwnd: isize,
    tx: Sender<Job>,
    thread: Option<JoinHandle<()>>,
}

impl ShellHost {
    /// Démarre le thread hôte et attend que sa fenêtre soit créée.
    pub fn start(name: &str) -> Result<Self, String> {
        let (tx, rx) = channel::<Job>();
        let (ready_tx, ready_rx) = channel::<Result<isize, String>>();
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || host_main(rx, ready_tx))
            .map_err(|e| e.to_string())?;
        let hwnd = ready_rx.recv().map_err(|e| e.to_string())??;
        Ok(Self {
            hwnd,
            tx,
            thread: Some(thread),
        })
    }

    /// Exécute `job` sur le thread hôte. Renvoie `false` si l'hôte est arrêté.
    pub fn post(&self, job: impl FnOnce(&HostCtx) + Send + 'static) -> bool {
        if self.tx.send(Box::new(job)).is_err() {
            return false;
        }
        // SAFETY: HWND de la fenêtre de l'hôte, vivante tant que `self` existe ; PostMessageW
        // n'accède à aucune mémoire de l'appelant.
        unsafe { PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_RUN_JOBS, WPARAM(0), LPARAM(0)) }.is_ok()
    }
}

impl Drop for ShellHost {
    fn drop(&mut self) {
        // SAFETY: voir `post`. WM_CLOSE détruit la fenêtre, ce qui termine la boucle.
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn host_main(rx: Receiver<Job>, ready: Sender<Result<isize, String>>) {
    // SAFETY: initialisation OLE (STA) du thread courant, équilibrée par OleUninitialize en fin
    // de fonction, sur le même thread.
    if let Err(e) = unsafe { OleInitialize(None) } {
        let _ = ready.send(Err(format!("OleInitialize : {e}")));
        return;
    }
    JOBS.with_borrow_mut(|j| *j = Some(rx));
    match create_window() {
        Ok(hwnd) => {
            let _ = ready.send(Ok(hwnd.0 as isize));
            let mut msg = MSG::default();
            // SAFETY: boucle de messages standard sur des MSG locaux.
            unsafe {
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        Err(e) => {
            let _ = ready.send(Err(format!("création de la fenêtre hôte : {e}")));
        }
    }
    JOBS.with_borrow_mut(|j| *j = None);
    set_active_menu(None);
    // SAFETY: conserve le contenu du presse-papiers placé par ce thread après sa fin, puis
    // équilibre OleInitialize.
    unsafe {
        let _ = OleFlushClipboard();
        OleUninitialize();
    }
}

fn create_window() -> windows::core::Result<HWND> {
    // SAFETY: enregistrement d'une classe de fenêtre avec une procédure `extern "system"`
    // statique ; le nom de classe est une constante. Un double enregistrement échoue sans
    // conséquence (on crée la fenêtre quand même).
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("WinDirShellHost"),
            ..Default::default()
        };
        RegisterClassExW(&class);
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("WinDirShellHost"),
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}

fn run_jobs(hwnd: HWND) {
    let ctx = HostCtx { hwnd };
    loop {
        // On ne garde pas l'emprunt pendant l'exécution : une tâche peut relancer une boucle
        // modale qui ré-entre dans `run_jobs`.
        let job = JOBS.with_borrow(|j| j.as_ref().and_then(|rx| rx.try_recv().ok()));
        let Some(job) = job else { break };
        if std::panic::catch_unwind(AssertUnwindSafe(|| job(&ctx))).is_err() {
            tracing::error!("panic dans une tâche de l'hôte Shell");
        }
    }
}

/// Relaie un message de menu au menu contextuel actif. `Some(résultat)` si traité.
fn forward_menu_message(msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let menu = ACTIVE_MENU.with_borrow(|m| m.clone())?;
    if let Ok(m3) = menu.cast::<IContextMenu3>() {
        let mut result = LRESULT(0);
        // SAFETY: interface COM vivante (clone possédé) ; `result` est un LRESULT local.
        if unsafe { m3.HandleMenuMsg2(msg, wparam, lparam, Some(&mut result)) }.is_ok() {
            return Some(result);
        }
    } else if let Ok(m2) = menu.cast::<IContextMenu2>() {
        // SAFETY: interface COM vivante.
        if unsafe { m2.HandleMenuMsg(msg, wparam, lparam) }.is_ok() {
            return Some(LRESULT(0));
        }
    }
    None
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_RUN_JOBS => {
            run_jobs(hwnd);
            return LRESULT(0);
        }
        WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR => {
            let r = std::panic::catch_unwind(|| forward_menu_message(msg, wparam, lparam));
            if let Ok(Some(r)) = r {
                return r;
            }
        }
        WM_TIMER if wparam.0 == TIMER_DISMISS_MENU => {
            // SAFETY: minuterie posée par `context_menu::show` sur cette fenêtre.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_DISMISS_MENU);
                let _ = EndMenu();
            }
            return LRESULT(0);
        }
        WM_CLOSE => {
            // SAFETY: fenêtre de ce thread.
            let _ = unsafe { DestroyWindow(hwnd) };
            return LRESULT(0);
        }
        WM_DESTROY => {
            // SAFETY: termine la boucle de messages de ce thread.
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: traitement par défaut avec les paramètres reçus.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
