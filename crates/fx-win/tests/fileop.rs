//! `IFileOperation` sur des fichiers réels (mode silencieux, dossier temporaire).
//! La suppression vers la corbeille n'est pas testée pour ne pas remplir celle de l'utilisateur.

use std::ffi::OsString;

use fx_win::com::ComApartment;
use fx_win::fileop::{FileOp, OpOptions, perform};

type Res = Result<(), Box<dyn std::error::Error>>;

const SILENT: OpOptions = OpOptions {
    owner: 0,
    silent: true,
};

#[test]
fn rename_new_folder_copy_move_delete() -> Res {
    let _com = ComApartment::sta()?;
    let dir = tempfile::tempdir()?;
    let a = dir.path().join("a.txt");
    std::fs::write(&a, b"contenu")?;

    // Renommer.
    let out = perform(
        &FileOp::Rename {
            item: a.clone(),
            new_name: OsString::from("b.txt"),
        },
        SILENT,
    )?;
    let b = dir.path().join("b.txt");
    assert!(b.exists() && !a.exists());
    assert_eq!(out.created.len(), 1);

    // Nouveau dossier, deux fois : la collision donne « (2) ».
    let nf = |name: &str| FileOp::NewFolder {
        parent: dir.path().to_path_buf(),
        name: OsString::from(name),
    };
    let first = perform(&nf("Nouveau dossier"), SILENT)?;
    let second = perform(&nf("Nouveau dossier"), SILENT)?;
    assert!(dir.path().join("Nouveau dossier").is_dir());
    assert_eq!(first.created.len(), 1);
    assert_eq!(second.created.len(), 1);
    assert_ne!(
        first.created[0], second.created[0],
        "le second dossier doit être renommé"
    );
    assert!(second.created[0].is_dir());

    // Copier sur place : copie renommée, pas de conflit.
    let copy = perform(
        &FileOp::Copy {
            items: vec![b.clone()],
            dest: dir.path().to_path_buf(),
        },
        SILENT,
    )?;
    assert_eq!(copy.created.len(), 1);
    assert_ne!(copy.created[0], b);
    assert_eq!(std::fs::read(&copy.created[0])?, b"contenu");

    // Déplacer dans le nouveau dossier.
    let target = dir.path().join("Nouveau dossier");
    perform(
        &FileOp::Move {
            items: vec![b.clone()],
            dest: target.clone(),
        },
        SILENT,
    )?;
    assert!(!b.exists() && target.join("b.txt").exists());

    // Suppression définitive.
    perform(
        &FileOp::Delete {
            items: vec![target.clone()],
            permanent: true,
        },
        SILENT,
    )?;
    assert!(!target.exists());
    Ok(())
}

#[test]
fn unicode_names_survive_operations() -> Res {
    let _com = ComApartment::sta()?;
    let dir = tempfile::tempdir()?;
    let src = dir.path().join("été 😀.txt");
    std::fs::write(&src, b"x")?;
    perform(
        &FileOp::Rename {
            item: src,
            new_name: OsString::from("évolution 日本.txt"),
        },
        SILENT,
    )?;
    assert!(dir.path().join("évolution 日本.txt").exists());
    Ok(())
}
