//! Menu contextuel natif du Shell (`IContextMenu` / `IContextMenu3`), extensions tierces
//! comprises, plus nos propres entrées. S'exécute sur l'hôte Shell ([`crate::shell_host`]).

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::Threading::AttachThreadInput;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_SHIFT};
use windows::Win32::UI::Shell::{
    BHID_SFObject, CMF_CANRENAME, CMF_EXPLORE, CMF_EXTENDEDVERBS, CMF_NORMAL, CMIC_MASK_PTINVOKE,
    CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX, GCS_VERBW, IContextMenu, IContextMenu3, IShellFolder,
    IShellItem, SHCreateItemFromParsingName,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyMenu, GetMenuItemCount, GetMenuStringW, GetSubMenu, GetWindowThreadProcessId,
    HMENU, MF_BYPOSITION, SW_SHOWNORMAL, SetForegroundWindow, SetTimer, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenuEx, WM_INITMENUPOPUP,
};
use windows::core::{PCSTR, PCWSTR, PSTR};

use crate::path::shell_path;
use crate::pidl::ui_object_of;
use crate::shell_host::{HostCtx, TIMER_DISMISS_MENU, set_active_menu};

const ID_FIRST: u32 = 1;
const ID_LAST: u32 = 0x7EFF;
// Les identifiants au-delà de ID_LAST sont réservés à nos propres entrées (phase 4 : « Ouvrir
// dans le terminal »). Windows 11 fournit déjà « Copier en tant que chemin d'accès ».
/// `CMIC_MASK_UNICODE` (= `SEE_MASK_UNICODE`, shobjidl.h), absente du crate `windows` 0.62.
const CMIC_MASK_UNICODE: u32 = 0x0000_4000;

/// Commandes gérées par l'application plutôt que par le Shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppCommand {
    /// Le Shell propose « Renommer » : on renomme en place, comme l'Explorateur.
    Rename,
}

#[derive(Clone, Debug, Default)]
pub struct MenuOutcome {
    /// Verbe Shell exécuté (ex. « properties », « open »), ou vide si inconnu.
    pub invoked: Option<String>,
    pub app_command: Option<AppCommand>,
    /// Libellés du premier niveau, pour le diagnostic.
    pub items: Vec<String>,
}

/// Options d'affichage.
#[derive(Clone, Copy, Debug, Default)]
pub struct MenuOptions {
    /// Position écran.
    pub x: i32,
    pub y: i32,
    /// Fenêtre principale : propriétaire des dialogues lancés par les verbes.
    pub owner: isize,
    /// Ferme automatiquement le menu après ce délai (diagnostic, sans entrée synthétique).
    pub auto_dismiss_ms: Option<u32>,
}

fn context_menu_for(owner: HWND, folder: &Path, items: &[PathBuf]) -> windows::core::Result<IContextMenu> {
    if items.is_empty() {
        // Menu de fond de dossier (Nouveau, Coller, Propriétés…).
        let wide = shell_path(folder);
        // SAFETY: chaîne terminée par un nul ; objets COM possédés.
        unsafe {
            let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None)?;
            let sf: IShellFolder = item.BindToHandler(None, &BHID_SFObject)?;
            sf.CreateViewObject::<IContextMenu>(owner)
        }
    } else {
        let refs: Vec<&Path> = items.iter().map(PathBuf::as_path).collect();
        ui_object_of::<IContextMenu>(owner, &refs)
    }
}

fn menu_strings(menu: HMENU) -> Vec<String> {
    // SAFETY: HMENU valide pendant l'appel ; tampon local dont la longueur est transmise.
    unsafe {
        let n = GetMenuItemCount(Some(menu)).max(0) as u32;
        (0..n)
            .map(|i| {
                let mut buf = [0u16; 256];
                let len = GetMenuStringW(menu, i, Some(&mut buf), MF_BYPOSITION).max(0) as usize;
                String::from_utf16_lossy(&buf[..len.min(buf.len())])
            })
            .collect()
    }
}

fn verb_of(menu: &IContextMenu, offset: u32) -> Option<String> {
    let mut buf = [0u16; 256];
    // SAFETY: avec GCS_VERBW, la sortie est une chaîne UTF-16 ; on passe un tampon u16 local
    // réinterprété en PSTR, de capacité annoncée en caractères.
    unsafe {
        menu.GetCommandString(
            offset as usize,
            GCS_VERBW,
            None,
            PSTR(buf.as_mut_ptr().cast()),
            buf.len() as u32,
        )
        .ok()?;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn query(menu: &IContextMenu, hmenu: HMENU, has_items: bool) -> windows::core::Result<()> {
    // SAFETY: GetKeyState sans précondition.
    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    let mut flags = CMF_NORMAL | CMF_EXPLORE;
    if has_items {
        flags |= CMF_CANRENAME;
    }
    if shift {
        flags |= CMF_EXTENDEDVERBS;
    }
    // SAFETY: HMENU possédé ; plage d'identifiants réservée au Shell.
    unsafe { menu.QueryContextMenu(hmenu, 0, ID_FIRST, ID_LAST, flags) }.ok()
}

/// Affiche le menu contextuel de `items` (ou du fond de `folder` si vide) et exécute le choix.
pub fn show(
    ctx: &HostCtx,
    folder: &Path,
    items: &[PathBuf],
    opts: MenuOptions,
) -> windows::core::Result<MenuOutcome> {
    let owner = HWND(opts.owner as *mut _);
    let menu = context_menu_for(owner, folder, items)?;
    // SAFETY: création d'un menu possédé, détruit en fin de fonction.
    let hmenu = unsafe { CreatePopupMenu()? };
    let result = (|| {
        query(&menu, hmenu, !items.is_empty())?;
        let mut outcome = MenuOutcome {
            items: menu_strings(hmenu),
            ..Default::default()
        };

        set_active_menu(Some(menu.clone()));
        // Le menu appartient à la fenêtre cachée de l'hôte : on partage la file d'entrée avec
        // le thread de la fenêtre principale pour qu'il puisse prendre le premier plan et se
        // fermer normalement au clic extérieur.
        // SAFETY: appels sans pointeur ; HWND valides ou ignorés par le système.
        let cmd = unsafe {
            let ui_thread = GetWindowThreadProcessId(owner, None);
            let me = GetCurrentThreadId();
            let attached = ui_thread != 0 && AttachThreadInput(me, ui_thread, true).as_bool();
            let _ = SetForegroundWindow(ctx.hwnd);
            if let Some(ms) = opts.auto_dismiss_ms {
                SetTimer(Some(ctx.hwnd), TIMER_DISMISS_MENU, ms, None);
            }
            let cmd = TrackPopupMenuEx(
                hmenu,
                (TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
                opts.x,
                opts.y,
                ctx.hwnd,
                None,
            )
            .0 as u32;
            if attached {
                let _ = AttachThreadInput(me, ui_thread, false);
            }
            let _ = SetForegroundWindow(owner);
            cmd
        };
        set_active_menu(None);

        if (ID_FIRST..=ID_LAST).contains(&cmd) {
            let offset = cmd - ID_FIRST;
            let verb = verb_of(&menu, offset);
            if verb.as_deref() == Some("rename") {
                outcome.app_command = Some(AppCommand::Rename);
            } else {
                invoke(&menu, offset, owner, folder, opts)?;
                outcome.invoked = Some(verb.unwrap_or_default());
            }
        }
        Ok(outcome)
    })();
    // SAFETY: menu créé ci-dessus, détruit une seule fois.
    unsafe {
        let _ = DestroyMenu(hmenu);
    }
    result
}

fn invoke(
    menu: &IContextMenu,
    offset: u32,
    owner: HWND,
    folder: &Path,
    opts: MenuOptions,
) -> windows::core::Result<()> {
    let dir = shell_path(folder);
    let info = CMINVOKECOMMANDINFOEX {
        cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        fMask: CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE,
        hwnd: owner,
        // MAKEINTRESOURCE : l'identifiant relatif est passé à la place d'un pointeur.
        lpVerb: PCSTR(offset as usize as *const u8),
        lpVerbW: PCWSTR(offset as usize as *const u16),
        lpDirectoryW: PCWSTR(dir.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ptInvoke: POINT { x: opts.x, y: opts.y },
        ..Default::default()
    };
    // SAFETY: structure étendue complète (cbSize correct) passée via un pointeur vers la
    // structure de base, comme le prévoit l'API ; `dir` vit jusqu'au retour.
    unsafe { menu.InvokeCommand((&info as *const CMINVOKECOMMANDINFOEX).cast::<CMINVOKECOMMANDINFO>()) }
}

/// Diagnostic sans affichage : libellés du menu et, pour chaque sous-menu, le nombre d'entrées
/// après relais de `WM_INITMENUPOPUP` (vérifie que « Envoyer vers » se remplit).
pub fn describe(
    ctx: &HostCtx,
    folder: &Path,
    items: &[PathBuf],
) -> windows::core::Result<Vec<(String, Option<usize>)>> {
    let menu = context_menu_for(ctx.hwnd, folder, items)?;
    // SAFETY: menu possédé, détruit en fin de fonction.
    let hmenu = unsafe { CreatePopupMenu()? };
    let result = (|| {
        query(&menu, hmenu, !items.is_empty())?;
        let labels = menu_strings(hmenu);
        let m3 = windows::core::Interface::cast::<IContextMenu3>(&menu).ok();
        let mut out = Vec::new();
        for (i, label) in labels.into_iter().enumerate() {
            // SAFETY: HMENU valide ; GetSubMenu renvoie un HMENU nul s'il n'y a pas de sous-menu.
            let sub = unsafe { GetSubMenu(hmenu, i as i32) };
            let count = if sub.is_invalid() {
                None
            } else {
                if let Some(m3) = &m3 {
                    // SAFETY: relais du message que la boucle de menu enverrait à l'ouverture.
                    let _ = unsafe {
                        m3.HandleMenuMsg2(WM_INITMENUPOPUP, WPARAM(sub.0 as usize), LPARAM(i as isize), None)
                    };
                }
                // SAFETY: sous-menu valide.
                Some(unsafe { GetMenuItemCount(Some(sub)) }.max(0) as usize)
            };
            out.push((label, count));
        }
        Ok(out)
    })();
    // SAFETY: détruit une fois.
    unsafe {
        let _ = DestroyMenu(hmenu);
    }
    result
}
