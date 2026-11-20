//! Opérations sur les fichiers via `IFileOperation` : corbeille, annulation dans l'Explorateur,
//! conflits, élévation et progression gérés par Windows.
//!
//! Aucune opération destructive de l'application ne contourne ce module.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, IDataObject};
use windows::Win32::UI::Shell::{
    COPYENGINE_E_USER_CANCELLED, FILEOPERATION_FLAGS, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOCONFIRMMKDIR,
    FOF_NOERRORUI, FOF_RENAMEONCOLLISION, FOF_SILENT, FOFX_RECYCLEONDELETE, FileOperation, IFileOperation,
    IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem, IShellItemArray,
    SHCreateItemFromParsingName, SHCreateShellItemArrayFromIDLists, SIGDN_FILESYSPATH,
};
use windows::core::{HRESULT, PCWSTR, Ref, implement};

use crate::path::{display_form, wide_nul};
use crate::pidl::Pidl;

/// Opération demandée.
#[derive(Clone, Debug)]
pub enum FileOp {
    /// Vers la corbeille, ou définitive (Windows demande alors confirmation).
    Delete {
        items: Vec<PathBuf>,
        permanent: bool,
    },
    Rename {
        item: PathBuf,
        new_name: OsString,
    },
    /// Crée un dossier ; un suffixe « (2) » est ajouté en cas de collision.
    NewFolder {
        parent: PathBuf,
        name: OsString,
    },
    Copy {
        items: Vec<PathBuf>,
        dest: PathBuf,
    },
    Move {
        items: Vec<PathBuf>,
        dest: PathBuf,
    },
}

/// Options d'exécution.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpOptions {
    /// HWND parent des dialogues (progression, conflits, confirmation).
    pub owner: isize,
    /// Pour les tests : aucune interface (ni progression, ni confirmation, ni erreur).
    pub silent: bool,
}

/// Résultat d'une opération.
#[derive(Clone, Debug, Default)]
pub struct OpOutcome {
    /// L'utilisateur a annulé tout ou partie de l'opération.
    pub aborted: bool,
    /// Éléments créés (nouveau dossier, copies, éléments renommés ou déplacés).
    pub created: Vec<PathBuf>,
}

fn shell_item(path: &Path) -> windows::core::Result<IShellItem> {
    let wide = wide_nul(display_form(path).as_os_str());
    // SAFETY: chaîne terminée par un nul vivante pendant l'appel.
    unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None) }
}

fn item_array(paths: &[PathBuf]) -> windows::core::Result<IShellItemArray> {
    let pidls = paths
        .iter()
        .map(|p| Pidl::parse(p))
        .collect::<windows::core::Result<Vec<_>>>()?;
    let raw: Vec<_> = pidls.iter().map(|p| p.as_ptr()).collect();
    // SAFETY: PIDL absolus valides, possédés par `pidls` jusqu'au retour (l'API les copie).
    unsafe { SHCreateShellItemArrayFromIDLists(&raw) }
}

/// Récepteur de progression : collecte les éléments créés.
#[implement(IFileOperationProgressSink)]
struct Sink {
    created: Rc<RefCell<Vec<PathBuf>>>,
}

impl Sink {
    fn record(&self, item: Ref<'_, IShellItem>) {
        let Some(item) = item.as_ref() else { return };
        // SAFETY: interface COM valide fournie par l'appelant ; la chaîne renvoyée est allouée
        // par le Shell et libérée par CoTaskMemFree après copie.
        unsafe {
            if let Ok(p) = item.GetDisplayName(SIGDN_FILESYSPATH) {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                self.created.borrow_mut().push(PathBuf::from(s));
            }
        }
    }
}

#[allow(non_snake_case)]
impl IFileOperationProgressSink_Impl for Sink_Impl {
    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn FinishOperations(&self, _: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostRenameItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        hr: HRESULT,
        created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        if hr.is_ok() {
            self.record(created);
        }
        Ok(())
    }
    fn PreMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        hr: HRESULT,
        created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        if hr.is_ok() {
            self.record(created);
        }
        Ok(())
    }
    fn PreCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        hr: HRESULT,
        created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        if hr.is_ok() {
            self.record(created);
        }
        Ok(())
    }
    fn PreDeleteItem(&self, _: u32, _: Ref<IShellItem>) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostDeleteItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostNewItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: &PCWSTR,
        _: u32,
        hr: HRESULT,
        created: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        if hr.is_ok() {
            self.record(created);
        }
        Ok(())
    }
    fn UpdateProgress(&self, _: u32, _: u32) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Source d'une copie ou d'un déplacement : chemins, ou objet de données (presse-papiers,
/// glisser-déposer), qui permet aussi de copier des éléments virtuels.
pub enum Source<'a> {
    Paths(&'a [PathBuf]),
    Data(&'a IDataObject),
}

fn new_operation(opts: OpOptions, extra: FILEOPERATION_FLAGS) -> windows::core::Result<IFileOperation> {
    // SAFETY: création d'un objet COM in-process sur un thread initialisé (exigence documentée
    // de `perform`).
    let fo: IFileOperation = unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL)? };
    let mut flags = FOF_NOCONFIRMMKDIR | extra;
    if opts.silent {
        flags |= FOF_SILENT | FOF_NOCONFIRMATION | FOF_NOERRORUI;
    }
    // SAFETY: appels de configuration sur un objet COM valide.
    unsafe {
        if opts.owner != 0 {
            fo.SetOwnerWindow(HWND(opts.owner as *mut _))?;
        }
        fo.SetOperationFlags(flags)?;
    }
    Ok(fo)
}

fn run(fo: &IFileOperation) -> windows::core::Result<OpOutcome> {
    let created = Rc::new(RefCell::new(Vec::new()));
    let sink: IFileOperationProgressSink = Sink {
        created: created.clone(),
    }
    .into();
    // SAFETY: objets COM valides ; le cookie d'abonnement est libéré par Unadvise.
    unsafe {
        let cookie = fo.Advise(&sink)?;
        let performed = fo.PerformOperations();
        let _ = fo.Unadvise(cookie);
        let aborted = fo.GetAnyOperationsAborted().map(|b| b.as_bool()).unwrap_or(false);
        let cancelled = |e: &windows::core::Error| {
            e.code() == COPYENGINE_E_USER_CANCELLED || e.code() == ERROR_CANCELLED.to_hresult()
        };
        match performed {
            Ok(()) => {}
            Err(e) if cancelled(&e) => {
                return Ok(OpOutcome {
                    aborted: true,
                    created: Vec::new(),
                });
            }
            Err(e) => return Err(e),
        }
        let created = created.take();
        Ok(OpOutcome { aborted, created })
    }
}

/// Exécute `op`. À appeler sur un thread STA initialisé (COM ou OLE), hors du thread
/// d'interface : l'opération peut durer et afficher des dialogues.
pub fn perform(op: &FileOp, opts: OpOptions) -> windows::core::Result<OpOutcome> {
    match op {
        FileOp::Delete { items, permanent } => {
            let extra = if *permanent {
                FILEOPERATION_FLAGS(0)
            } else {
                FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE
            };
            let fo = new_operation(opts, extra)?;
            let array = item_array(items)?;
            // SAFETY: objets COM valides.
            unsafe { fo.DeleteItems(&array)? };
            run(&fo)
        }
        FileOp::Rename { item, new_name } => {
            let fo = new_operation(opts, FOF_ALLOWUNDO)?;
            let it = shell_item(item)?;
            let name = wide_nul(new_name);
            // SAFETY: objets COM valides, chaîne terminée par un nul vivante jusqu'au retour de
            // PerformOperations (dans `run`).
            unsafe { fo.RenameItem(&it, PCWSTR(name.as_ptr()), None)? };
            run(&fo)
        }
        FileOp::NewFolder { parent, name } => {
            let fo = new_operation(opts, FOF_ALLOWUNDO | FOF_RENAMEONCOLLISION)?;
            let dest = shell_item(parent)?;
            let name = wide_nul(name);
            // SAFETY: voir Rename.
            unsafe {
                fo.NewItem(
                    &dest,
                    FILE_ATTRIBUTE_DIRECTORY.0,
                    PCWSTR(name.as_ptr()),
                    PCWSTR::null(),
                    None,
                )?
            };
            run(&fo)
        }
        FileOp::Copy { items, dest } => copy_or_move(Source::Paths(items), dest, false, opts),
        FileOp::Move { items, dest } => copy_or_move(Source::Paths(items), dest, true, opts),
    }
}

/// Copie ou déplace `source` dans `dest`. Si tous les éléments viennent déjà de `dest`, les
/// copies sont renommées (« - Copie ») au lieu de provoquer un conflit, comme dans l'Explorateur.
pub fn copy_or_move(
    source: Source<'_>,
    dest: &Path,
    is_move: bool,
    opts: OpOptions,
) -> windows::core::Result<OpOutcome> {
    let same_folder = match &source {
        Source::Paths(p) => !p.is_empty() && p.iter().all(|i| i.parent() == Some(dest)),
        Source::Data(_) => false,
    };
    if is_move && same_folder {
        return Ok(OpOutcome::default()); // déplacer un élément sur place : rien à faire
    }
    let extra = if same_folder {
        FOF_ALLOWUNDO | FOF_RENAMEONCOLLISION
    } else {
        FOF_ALLOWUNDO
    };
    let fo = new_operation(opts, extra)?;
    let dest = shell_item(dest)?;
    // SAFETY: objets COM valides.
    unsafe {
        match source {
            Source::Paths(p) => {
                let array = item_array(p)?;
                if is_move {
                    fo.MoveItems(&array, &dest)?
                } else {
                    fo.CopyItems(&array, &dest)?
                }
            }
            Source::Data(d) => {
                if is_move {
                    fo.MoveItems(d, &dest)?
                } else {
                    fo.CopyItems(d, &dest)?
                }
            }
        }
    }
    run(&fo)
}
