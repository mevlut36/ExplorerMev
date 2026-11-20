//! Chargeur complet (listing Windows, types, collation, tri) sur des arborescences piégées.

use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fx_core::folder::Folder;
use fx_core::loader::{LoadEvent, Loader, Sink};
use fx_core::sort::SortSpec;
use fx_core::source::ErrorKind;
use fx_core::sys::{WinCollator, WinSource};

type Res = Result<(), Box<dyn std::error::Error>>;

enum Outcome {
    Done(Arc<Folder>, Vec<u32>),
    Failed(ErrorKind),
}

fn load(path: &Path) -> Result<Outcome, Box<dyn std::error::Error>> {
    let loader = Loader::new(Arc::new(WinSource::default()), Arc::new(WinCollator));
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let sink: Sink = Arc::new(move |e| {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(e);
        }
    });
    loader.load(path.to_path_buf(), 1, SortSpec::default(), sink, None);
    loop {
        match rx.recv_timeout(Duration::from_secs(20))? {
            LoadEvent::Complete { folder, order, .. } => return Ok(Outcome::Done(folder, order)),
            LoadEvent::Failed { error, .. } => return Ok(Outcome::Failed(error.kind)),
            LoadEvent::Partial { .. } => {}
        }
    }
}

fn names(folder: &Folder, order: &[u32]) -> Vec<String> {
    order.iter().map(|&i| folder.display_name(i)).collect()
}

#[test]
fn loads_a_trapped_tree_end_to_end() -> Res {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    // Noms Unicode et tri naturel.
    for n in [
        "fichier 10.txt",
        "fichier 2.txt",
        "été.md",
        "Zèbre.pdf",
        "😀 émoji.png",
    ] {
        std::fs::write(root.join(n), b"x")?;
    }
    std::fs::create_dir(root.join("Dossier"))?;
    // Jonction en boucle vers son propre parent.
    let ok = Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(root.join("boucle"))
        .arg(root)
        .output()?
        .status
        .success();
    assert!(ok);
    // Fichier verrouillé en exclusivité.
    std::fs::write(root.join("verrou.bin"), b"y")?;
    let _lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(root.join("verrou.bin"))?;

    let Outcome::Done(folder, order) = load(root)? else {
        panic!("échec du chargement")
    };
    let got = names(&folder, &order);
    // Référence : dossiers en premier, puis fichiers dans l'ordre de StrCmpLogicalW
    // (la comparaison de l'Explorateur).
    let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    let mut files = vec![
        "fichier 10.txt",
        "fichier 2.txt",
        "été.md",
        "Zèbre.pdf",
        "😀 émoji.png",
        "verrou.bin",
    ];
    files.sort_by(|a, b| fx_win::locale::str_cmp_logical(&w(a), &w(b)));
    let mut dirs = vec!["boucle", "Dossier"];
    dirs.sort_by(|a, b| fx_win::locale::str_cmp_logical(&w(a), &w(b)));
    let expected: Vec<&str> = dirs.into_iter().chain(files).collect();
    assert_eq!(got, expected, "dossiers en premier, tri de l'Explorateur");
    // La jonction est un dossier, reconnue par son tag, sans être suivie.
    let j = folder
        .find(&"boucle".encode_utf16().collect::<Vec<_>>())
        .ok_or("jonction absente")?;
    assert_eq!(folder.entry(j).map(|e| e.reparse_tag), Some(0xA000_0003));
    // Types résolus par le Shell.
    let t = folder
        .find(&"été.md".encode_utf16().collect::<Vec<_>>())
        .ok_or("été.md absent")?;
    assert!(folder.entry(t).is_some_and(|e| !folder.type_label(e).is_empty()));
    Ok(())
}

#[test]
fn long_path_folder_loads() -> Res {
    let dir = tempfile::tempdir()?;
    let mut deep: PathBuf = dir.path().to_path_buf();
    while deep.as_os_str().len() < 330 {
        deep.push("segment-de-chemin-assez-long");
    }
    std::fs::create_dir_all(&deep)?;
    std::fs::write(deep.join("profond.txt"), b"z")?;
    let Outcome::Done(folder, order) = load(&deep)? else {
        panic!("échec")
    };
    assert_eq!(names(&folder, &order), ["profond.txt"]);
    Ok(())
}

#[test]
fn errors_reach_the_loader_classified() -> Res {
    let dir = tempfile::tempdir()?;
    assert!(matches!(
        load(&dir.path().join("absent"))?,
        Outcome::Failed(ErrorKind::NotFound)
    ));
    let file = dir.path().join("f.txt");
    std::fs::write(&file, b"")?;
    assert!(matches!(load(&file)?, Outcome::Failed(ErrorKind::NotADirectory)));
    let denied = dir.path().join("interdit");
    std::fs::create_dir(&denied)?;
    let user = std::env::var("USERNAME")?;
    Command::new("icacls")
        .arg(&denied)
        .arg("/deny")
        .arg(format!("{user}:(RD)"))
        .output()?;
    let r = load(&denied)?;
    Command::new("icacls")
        .arg(&denied)
        .arg("/remove:d")
        .arg(&user)
        .output()?;
    assert!(matches!(r, Outcome::Failed(ErrorKind::AccessDenied)));
    Ok(())
}
