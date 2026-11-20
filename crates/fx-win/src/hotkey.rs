//! Agent de raccourci : intercepte **Windows+E** et ouvre ExplorerMev à la place de l'Explorateur.
//!
//! Sur les versions récentes de Windows 11, le raccourci Windows+E est traité par `explorer.exe`
//! lui-même : la commande `opennewwindow` de l'utilisateur n'est pas consultée. Comme les autres
//! gestionnaires de fichiers, on capture donc le clavier (crochet `WH_KEYBOARD_LL`) dans un petit
//! processus résident (`explorermev --hotkey-agent`) qui :
//! - ne réagit qu'à **Win+E sans autre modificateur** et ignore les touches injectées ;
//! - lance ExplorerMev **avant** d'avaler la touche : si le lancement échoue, la touche continue
//!   vers Windows (le raccourci n'est jamais perdu) ;
//! - envoie une touche neutre (`VK 0xE8`) pour que le relâchement de la touche Windows n'ouvre pas
//!   le menu Démarrer ;
//! - ne s'exécute qu'en une instance (mutex) et s'arrête sur demande (`stop_agent`).
//!
//! Le crochet est retiré par Windows si le processus s'arrête : aucun état persistant.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
    SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowExW, GetMessageW, HHOOK,
    HWND_MESSAGE, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_CLOSE, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSW,
};
use windows::core::{PCWSTR, w};

/// Classe de la fenêtre invisible qui permet d'arrêter l'agent.
const AGENT_CLASS: PCWSTR = w!("ExplorerMev.HotkeyAgent");
const AGENT_MUTEX: PCWSTR = w!("Local\\ExplorerMev-HotkeyAgent");
const VK_E: u32 = 0x45;
/// Touche sans effet (non attribuée) envoyée pour « masquer » la touche Windows.
const MASK_KEY: u16 = 0xE8;

/// Faut-il réagir à cette touche ? `vk` : code de touche ; `injected` : événement synthétique.
pub fn is_win_e(vk: u32, injected: bool, win: bool, ctrl: bool, shift: bool, alt: bool) -> bool {
    vk == VK_E && !injected && win && !ctrl && !shift && !alt
}

/// Action à lancer sur Win+E : renvoie `true` si le lancement a réussi.
static TRIGGER: OnceLock<fn() -> bool> = OnceLock::new();
/// La touche E est déjà enfoncée (répétition automatique : un seul lancement par appui).
static E_DOWN: AtomicBool = AtomicBool::new(false);

fn pressed(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: appel sans pointeur ; lit l'état asynchrone d'une touche.
    unsafe { GetAsyncKeyState(vk.0 as i32) < 0 }
}

/// Envoie une touche neutre (appui puis relâchement) : sans cela, relâcher la touche Windows
/// après avoir avalé le E ouvrirait le menu Démarrer.
fn send_mask_key() {
    let key = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(MASK_KEY),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)];
    // SAFETY: tableau local d'INPUT complets ; taille de la structure transmise.
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // `code` < 0 : le message doit être transmis sans traitement.
    if code >= 0 {
        // SAFETY: pour WH_KEYBOARD_LL, `lparam` pointe sur une KBDLLHOOKSTRUCT valide pendant
        // l'appel du crochet.
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let injected = kb.flags.0 & LLKHF_INJECTED.0 != 0;
        let msg = wparam.0 as u32;
        let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        if kb.vkCode == VK_E && !injected && (is_down || is_up) {
            let win = pressed(VK_LWIN) || pressed(VK_RWIN);
            if is_win_e(
                kb.vkCode,
                injected,
                win,
                pressed(VK_CONTROL),
                pressed(VK_SHIFT),
                pressed(VK_MENU),
            ) {
                if is_down && !E_DOWN.swap(true, Ordering::SeqCst) {
                    let launched = TRIGGER.get().is_some_and(|f| f());
                    if !launched {
                        // Échec du lancement : Windows garde le raccourci.
                        E_DOWN.store(false, Ordering::SeqCst);
                        // SAFETY: transmission normale du message au crochet suivant.
                        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
                    }
                    send_mask_key();
                }
                if is_up {
                    E_DOWN.store(false, Ordering::SeqCst);
                }
                return LRESULT(1);
            }
        }
        if kb.vkCode == VK_E && is_up {
            E_DOWN.store(false, Ordering::SeqCst);
        }
    }
    // SAFETY: transmission normale du message au crochet suivant.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn agent_window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_CLOSE {
        // SAFETY: demande d'arrêt de la boucle de messages du thread courant.
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    // SAFETY: traitement par défaut des messages qui ne nous concernent pas.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Exécute l'agent jusqu'à la demande d'arrêt. `Ok(false)` : une autre instance tourne déjà.
/// `trigger` lance ExplorerMev et renvoie `true` si le lancement a réussi.
pub fn run_agent(trigger: fn() -> bool) -> windows::core::Result<bool> {
    let _ = TRIGGER.set(trigger);
    // SAFETY: nom constant ; le handle de mutex est fermé en fin de fonction.
    let mutex = unsafe { CreateMutexW(None, true, AGENT_MUTEX)? };
    // SAFETY: lecture du dernier code d'erreur du thread, juste après CreateMutexW.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // SAFETY: handle valide, fermé une seule fois.
        unsafe {
            let _ = CloseHandle(mutex);
        }
        return Ok(false);
    }
    // SAFETY: fenêtre invisible (« message-only ») dont la procédure est définie ci-dessus ;
    // classe enregistrée pour la durée du processus ; crochet retiré avant de rendre la main.
    let result = unsafe {
        let hinstance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(agent_window_proc),
            hInstance: hinstance,
            lpszClassName: AGENT_CLASS,
            ..Default::default()
        };
        RegisterClassW(&class);
        let _window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            AGENT_CLASS,
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance),
            None,
        )?;
        let hook: HHOOK = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(hinstance), 0)?;
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = UnhookWindowsHookEx(hook);
        Ok(true)
    };
    // SAFETY: handle valide, fermé une seule fois.
    unsafe {
        let _ = CloseHandle(mutex);
    }
    result
}

fn agent_window() -> Option<HWND> {
    // SAFETY: recherche d'une fenêtre « message-only » par sa classe ; aucun pointeur conservé.
    unsafe { FindWindowExW(Some(HWND_MESSAGE), None, AGENT_CLASS, None).ok() }
}

/// Autorise le prochain processus qui le demande à prendre le premier plan. Windows interdit à un
/// processus lancé en arrière-plan de s'attribuer le focus ; l'agent, qui vient de recevoir la
/// frappe, a ce droit et peut le céder (jusqu'à la prochaine entrée de l'utilisateur).
pub fn allow_foreground() {
    use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
    // SAFETY: appel sans pointeur ; ASFW_ANY : valeur documentée pour « tout processus ».
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
}

/// L'agent tourne-t-il ?
pub fn is_agent_running() -> bool {
    agent_window().is_some()
}

/// Demande l'arrêt de l'agent. `true` s'il tournait.
pub fn stop_agent() -> bool {
    let Some(hwnd) = agent_window() else { return false };
    // SAFETY: message adressé à notre propre fenêtre d'agent.
    unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_win_e_is_captured() {
        assert!(is_win_e(0x45, false, true, false, false, false));
        assert!(!is_win_e(0x45, false, false, false, false, false), "E seul");
        assert!(
            !is_win_e(0x45, true, true, false, false, false),
            "touche injectée"
        );
        assert!(!is_win_e(0x44, false, true, false, false, false), "Win+D");
        assert!(!is_win_e(0x45, false, true, true, false, false), "Ctrl+Win+E");
        assert!(!is_win_e(0x45, false, true, false, true, false), "Maj+Win+E");
        assert!(!is_win_e(0x45, false, true, false, false, true), "Alt+Win+E");
    }

    #[test]
    fn no_agent_runs_during_tests() {
        assert!(!is_agent_running());
        assert!(!stop_agent());
    }
}
