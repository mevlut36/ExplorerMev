//! Tests d'intégration du listing sur des arborescences réelles générées dans un dossier temporaire.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::process::Command;

use fx_win::listing::{ListError, ListMethod, ListOptions, list_dir, list_dir_exact};

const METHODS: [ListMethod; 3] = [
    ListMethod::IdExtdDirectoryInfo,
    ListMethod::FullDirectoryInfo,
    ListMethod::FindFirstFileEx,
];
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

type Res = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct E {
    attributes: u32,
    reparse_tag: u32,
    size: u64,
    modified: i64,
}

fn collect(path: &Path, method: ListMethod) -> Result<BTreeMap<Vec<u16>, E>, ListError> {
    let mut out = BTreeMap::new();
    list_dir_exact(path, method, 64 * 1024, &mut |e| {
        out.insert(
            e.name.to_vec(),
            E {
                attributes: e.attributes,
                reparse_tag: e.reparse_tag,
                size: e.size,
                modified: e.modified,
            },
        );
    })?;
    Ok(out)
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

#[test]
fn all_methods_agree_on_a_mixed_folder() -> Res {
    let dir = tempfile::tempdir()?;
    for i in 0..300 {
        std::fs::write(dir.path().join(format!("fichier {i}.txt")), vec![b'x'; i])?;
    }
    for i in 0..20 {
        std::fs::create_dir(dir.path().join(format!("dossier {i}")))?;
    }
    let reference = collect(dir.path(), ListMethod::IdExtdDirectoryInfo)?;
    assert_eq!(reference.len(), 320);
    assert_eq!(reference[&w("fichier 42.txt")].size, 42);
    assert!(reference[&w("dossier 3")].attributes & FILE_ATTRIBUTE_DIRECTORY != 0);
    for m in METHODS {
        assert_eq!(collect(dir.path(), m)?, reference, "méthode {m:?}");
    }
    Ok(())
}

#[test]
fn tiny_buffer_still_lists_everything() -> Res {
    let dir = tempfile::tempdir()?;
    for i in 0..500 {
        std::fs::write(
            dir.path()
                .join(format!("un nom de fichier assez long numéro {i:04}.dat")),
            b"",
        )?;
    }
    let mut n = 0;
    list_dir_exact(dir.path(), ListMethod::IdExtdDirectoryInfo, 4096, &mut |_| n += 1)?;
    assert_eq!(n, 500);
    Ok(())
}

#[test]
fn empty_folder_has_no_entries() -> Res {
    let dir = tempfile::tempdir()?;
    for m in METHODS {
        assert!(collect(dir.path(), m)?.is_empty(), "méthode {m:?}");
    }
    Ok(())
}

#[test]
fn path_longer_than_max_path() -> Res {
    let dir = tempfile::tempdir()?;
    let mut deep = dir.path().to_path_buf();
    while deep.as_os_str().len() < 400 {
        deep.push("un-segment-de-chemin-plutot-long");
    }
    std::fs::create_dir_all(&deep)?;
    std::fs::write(deep.join("au fond.txt"), b"ok")?;
    assert!(deep.as_os_str().len() > 260);
    for m in METHODS {
        let got = collect(&deep, m)?;
        assert_eq!(got.len(), 1, "méthode {m:?}");
        assert!(got.contains_key(&w("au fond.txt")));
    }
    Ok(())
}

#[test]
fn exotic_unicode_names_round_trip() -> Res {
    let dir = tempfile::tempdir()?;
    let names: Vec<Vec<u16>> = vec![
        w("émoji 😀 📁"),
        w("e\u{0301}tude combinée"),
        w("中文目录"),
        w("ﾊﾝｶｸ"),
        w("עברית"),
        w(" espace initial"),
        // Substitut isolé : invalide en UTF-16, accepté par NTFS.
        vec![b'a' as u16, 0xD800, b'b' as u16],
    ];
    for n in &names {
        std::fs::write(dir.path().join(OsString::from_wide(n)), b"")?;
    }
    for m in METHODS {
        let got = collect(dir.path(), m)?;
        for n in &names {
            assert!(
                got.contains_key(n),
                "méthode {m:?}, nom {}",
                String::from_utf16_lossy(n)
            );
        }
    }
    Ok(())
}

#[test]
fn junction_is_reported_with_its_tag_without_being_followed() -> Res {
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("cible");
    std::fs::create_dir(&target)?;
    // Boucle : une jonction à l'intérieur de sa propre cible.
    let link = target.join("boucle");
    let ok = Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(&link)
        .arg(&target)
        .output()?
        .status
        .success();
    assert!(ok, "mklink /J a échoué");
    for m in METHODS {
        let got = collect(&target, m)?;
        let e = &got[&w("boucle")];
        assert_eq!(e.reparse_tag, IO_REPARSE_TAG_MOUNT_POINT, "méthode {m:?}");
    }
    Ok(())
}

#[test]
fn symlink_is_reported_with_its_tag() -> Res {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("cible.txt"), b"x")?;
    if std::os::windows::fs::symlink_file("cible.txt", dir.path().join("lien.txt")).is_err() {
        eprintln!("création de lien symbolique non autorisée (mode développeur désactivé) : test ignoré");
        return Ok(());
    }
    for m in METHODS {
        let got = collect(dir.path(), m)?;
        assert_eq!(
            got[&w("lien.txt")].reparse_tag,
            IO_REPARSE_TAG_SYMLINK,
            "méthode {m:?}"
        );
        assert_eq!(got[&w("cible.txt")].reparse_tag, 0);
    }
    Ok(())
}

#[test]
fn locked_file_is_listed() -> Res {
    let dir = tempfile::tempdir()?;
    let p = dir.path().join("verrouillé.bin");
    std::fs::write(&p, vec![0u8; 1234])?;
    let _lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(&p)?;
    for m in METHODS {
        assert_eq!(
            collect(dir.path(), m)?[&w("verrouillé.bin")].size,
            1234,
            "méthode {m:?}"
        );
    }
    Ok(())
}

#[test]
fn errors_are_classified() -> Res {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("pas-un-dossier.txt");
    std::fs::write(&file, b"")?;
    let noop = &mut |_: &fx_win::listing::RawEntry<'_>| {};
    let opts = ListOptions::default();
    assert!(matches!(
        list_dir(&dir.path().join("absent"), &opts, &|| false, noop),
        Err(ListError::NotFound)
    ));
    assert!(matches!(
        list_dir(&file, &opts, &|| false, noop),
        Err(ListError::NotADirectory)
    ));
    assert!(matches!(
        list_dir_exact(&file, ListMethod::FindFirstFileEx, 0, noop),
        Err(ListError::NotADirectory | ListError::NotFound)
    ));
    assert!(matches!(
        list_dir(dir.path(), &opts, &|| true, noop),
        Err(ListError::Cancelled)
    ));
    Ok(())
}

#[test]
fn access_denied_is_classified() -> Res {
    let dir = tempfile::tempdir()?;
    let locked = dir.path().join("interdit");
    std::fs::create_dir(&locked)?;
    let user = std::env::var("USERNAME")?;
    let deny = Command::new("icacls")
        .arg(&locked)
        .arg("/deny")
        .arg(format!("{user}:(RD)"))
        .output()?;
    assert!(deny.status.success(), "icacls /deny a échoué");
    let noop = &mut |_: &fx_win::listing::RawEntry<'_>| {};
    let results: Vec<_> = METHODS
        .iter()
        .map(|&m| list_dir_exact(&locked, m, 0, noop))
        .collect();
    // Nettoyage avant les assertions pour que le dossier temporaire puisse être supprimé.
    Command::new("icacls")
        .arg(&locked)
        .arg("/remove:d")
        .arg(&user)
        .output()?;
    for (m, r) in METHODS.iter().zip(results) {
        assert!(matches!(r, Err(ListError::AccessDenied)), "méthode {m:?} : {r:?}");
    }
    Ok(())
}

#[test]
fn volume_root_can_be_listed() -> Res {
    let root = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()) + "\\";
    for m in METHODS {
        let got = collect(Path::new(&root), m)?;
        assert!(got.contains_key(&w("Windows")), "méthode {m:?}");
    }
    Ok(())
}

#[test]
fn watcher_reports_add_rename_remove_in_batches() -> Res {
    use fx_win::watcher::{DirChange, watch};
    use std::sync::mpsc::channel;
    use std::time::Duration;
    let dir = tempfile::tempdir()?;
    let (tx, rx) = channel();
    let watcher = watch(dir.path(), move |batch| {
        let _ = tx.send(batch);
    })?;
    std::thread::sleep(Duration::from_millis(50));
    std::fs::write(dir.path().join("nouveau.txt"), b"x")?;
    std::fs::rename(dir.path().join("nouveau.txt"), dir.path().join("renommé.txt"))?;
    std::fs::remove_file(dir.path().join("renommé.txt"))?;
    let mut all = Vec::new();
    while let Ok(batch) = rx.recv_timeout(Duration::from_millis(1000)) {
        all.extend(batch);
    }
    drop(watcher);
    assert!(all.contains(&DirChange::Added(w("nouveau.txt"))), "{all:?}");
    assert!(all.contains(&DirChange::RenamedFrom(w("nouveau.txt"))), "{all:?}");
    assert!(all.contains(&DirChange::RenamedTo(w("renommé.txt"))), "{all:?}");
    assert!(all.contains(&DirChange::Removed(w("renommé.txt"))), "{all:?}");
    Ok(())
}

#[test]
fn watcher_reports_folder_deletion_and_stops_cleanly() -> Res {
    use fx_win::watcher::{DirChange, watch};
    use std::sync::mpsc::channel;
    use std::time::Duration;
    let parent = tempfile::tempdir()?;
    let dir = parent.path().join("surveillé");
    std::fs::create_dir(&dir)?;
    let (tx, rx) = channel();
    let watcher = watch(&dir, move |batch| {
        let _ = tx.send(batch);
    })?;
    std::thread::sleep(Duration::from_millis(50));
    std::fs::remove_dir(&dir)?;
    let mut got = Vec::new();
    while let Ok(batch) = rx.recv_timeout(Duration::from_millis(1000)) {
        got.extend(batch);
    }
    drop(watcher); // ne doit pas bloquer
    assert!(got.contains(&DirChange::Gone), "{got:?}");
    Ok(())
}

#[test]
fn stat_entry_reads_one_entry() -> Res {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("été.bin"), vec![0u8; 77])?;
    let e = fx_win::listing::stat_entry(dir.path(), &w("été.bin")).ok_or("absent")?;
    assert_eq!(e.size, 77);
    assert!(fx_win::listing::stat_entry(dir.path(), &w("absent")).is_none());
    Ok(())
}
