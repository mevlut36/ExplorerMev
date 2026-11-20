//! Tests du Shell sur l'hôte STA : menu contextuel (sans affichage).

use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::Duration;

use fx_win::context_menu::describe;
use fx_win::shell_host::ShellHost;

type Res = Result<(), Box<dyn std::error::Error>>;

fn on_host<T: Send + 'static>(
    host: &ShellHost,
    f: impl FnOnce(&fx_win::shell_host::HostCtx) -> T + Send + 'static,
) -> Result<T, Box<dyn std::error::Error>> {
    let (tx, rx) = channel();
    assert!(host.post(move |ctx| {
        let _ = tx.send(f(ctx));
    }));
    Ok(rx.recv_timeout(Duration::from_secs(30))?)
}

#[test]
fn file_context_menu_has_shell_entries_and_populated_submenus() -> Res {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("exemple.txt");
    std::fs::write(&file, b"x")?;
    let host = ShellHost::start("test-shell")?;
    let folder: PathBuf = dir.path().to_path_buf();
    let entries = on_host(&host, move |ctx| describe(ctx, &folder, &[file]))??;
    for (label, count) in &entries {
        println!("{label:?} -> {count:?}");
    }
    let labels: Vec<String> = entries.iter().map(|(l, _)| l.replace('&', "")).collect();
    assert!(
        labels
            .iter()
            .any(|l| l.contains("Propriétés") || l.contains("Properties")),
        "{labels:?}"
    );
    // Au moins un sous-menu dynamique (« Envoyer vers », « Ouvrir avec »…) se remplit via
    // le relais de WM_INITMENUPOPUP.
    assert!(
        entries.iter().any(|(_, c)| c.is_some_and(|n| n >= 2)),
        "{entries:?}"
    );
    Ok(())
}

#[test]
fn background_menu_is_available() -> Res {
    let dir = tempfile::tempdir()?;
    let host = ShellHost::start("test-shell-bg")?;
    let folder: PathBuf = dir.path().to_path_buf();
    let entries = on_host(&host, move |ctx| describe(ctx, &folder, &[]))??;
    println!("{entries:?}");
    assert!(!entries.is_empty());
    Ok(())
}

#[test]
fn icons_are_extracted_as_rgba() -> Res {
    let _com = fx_win::com::ComApartment::sta()?;
    let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    for (name, dir) in [
        ("a.txt", false),
        ("a.pdf", false),
        ("x", true),
        ("sans extension", false),
    ] {
        let img = fx_win::icons::type_icon(&w(name), dir, 16).ok_or("icône de type absente")?;
        assert!(
            img.width >= 16 && img.width == img.height,
            "{name} : {}x{}",
            img.width,
            img.height
        );
        assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
        // Une icône n'est ni totalement transparente ni totalement opaque.
        let alphas: Vec<u8> = img.rgba.iter().skip(3).step_by(4).copied().collect();
        assert!(
            alphas.iter().any(|&a| a > 0) && alphas.iter().any(|&a| a < 255),
            "{name}"
        );
    }
    let notepad = std::path::Path::new(r"C:\Windows\System32\notepad.exe");
    if notepad.exists() {
        let img = fx_win::icons::item_icon(notepad, 32).ok_or("icône de notepad.exe absente")?;
        assert_eq!(img.width, 32);
        assert!(img.rgba.iter().skip(3).step_by(4).any(|&a| a > 200));
    }
    Ok(())
}

#[test]
fn drop_target_receives_shell_data_object() -> Res {
    use fx_win::dragdrop::{DropHandler, EFFECT_COPY, EFFECT_MOVE, data_object, new_target};
    use std::sync::{Arc, Mutex};
    use windows::Win32::Foundation::POINTL;
    use windows::Win32::System::Ole::DROPEFFECT;
    use windows::Win32::System::SystemServices::{MK_CONTROL, MK_LBUTTON};

    type Drops = Arc<Mutex<Vec<(PathBuf, Vec<PathBuf>, u32)>>>;
    struct Recorder {
        dest: PathBuf,
        got: Drops,
    }
    impl DropHandler for Recorder {
        fn hover(&self, _x: i32, _y: i32) -> Option<PathBuf> {
            Some(self.dest.clone())
        }
        fn leave(&self) {}
        fn drop_files(&self, dest: PathBuf, paths: Vec<PathBuf>, effect: u32) {
            if let Ok(mut g) = self.got.lock() {
                g.push((dest, paths, effect));
            }
        }
    }

    let _ole = fx_win::com::OleApartment::init()?;
    let dir = tempfile::tempdir()?;
    let src = dir.path().join("source");
    let dest = dir.path().join("cible");
    std::fs::create_dir(&src)?;
    std::fs::create_dir(&dest)?;
    let files = vec![src.join("un.txt"), src.join("deux.txt")];
    for f in &files {
        std::fs::write(f, b"x")?;
    }
    let data = data_object(&files)?;
    assert_eq!(fx_win::dragdrop::files_of(&data).len(), 2);

    let got = Arc::new(Mutex::new(Vec::new()));
    let target = new_target(
        0,
        Box::new(Recorder {
            dest: dest.clone(),
            got: got.clone(),
        }),
    );
    let pt = POINTL { x: 10, y: 10 };
    let all = DROPEFFECT(EFFECT_COPY | EFFECT_MOVE);
    // SAFETY: appels COM comme OLE les ferait, avec des pointeurs locaux valides.
    unsafe {
        let mut effect = all;
        target.DragEnter(&data, MK_LBUTTON, pt, &mut effect)?;
        assert_eq!(effect.0, EFFECT_MOVE, "même volume, sans touche : déplacement");
        let mut effect = all;
        target.DragOver(MK_LBUTTON | MK_CONTROL, pt, &mut effect)?;
        assert_eq!(effect.0, EFFECT_COPY, "Ctrl : copie");
        let mut effect = all;
        target.Drop(&data, MK_CONTROL, pt, &mut effect)?;
    }
    let got = got.lock().map_err(|_| "verrou")?;
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0, dest);
    assert_eq!(got[0].2, EFFECT_COPY);
    let mut names: Vec<String> = got[0]
        .1
        .iter()
        .filter_map(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["deux.txt", "un.txt"]);
    Ok(())
}

/// Remplace le contenu du presse-papiers de l'utilisateur : lancé seulement à la demande,
/// avec `cargo test -p fx-win --test shell -- --ignored clipboard`.
#[test]
#[ignore = "modifie le presse-papiers de l'utilisateur"]
fn clipboard_copy_and_cut_paste_like_explorer() -> Res {
    use fx_win::fileop::OpOptions;
    let dir = tempfile::tempdir()?;
    let src = dir.path().join("source");
    let dest = dir.path().join("dest");
    std::fs::create_dir(&src)?;
    std::fs::create_dir(&dest)?;
    let file = src.join("presse-papiers.txt");
    std::fs::write(&file, b"x")?;
    let host = ShellHost::start("test-clipboard")?;
    let silent = OpOptions {
        owner: 0,
        silent: true,
    };

    // Copier puis coller : le fichier est copié, la source reste.
    let f = file.clone();
    on_host(&host, move |_| fx_win::clipboard::set_files(&[f], false))??;
    assert!(fx_win::clipboard::has_files());
    let _ole = fx_win::com::OleApartment::init()?;
    let out = fx_win::clipboard::paste(&dest, silent)?.ok_or("presse-papiers vide")?;
    assert_eq!(out.created.len(), 1);
    assert!(file.exists() && dest.join("presse-papiers.txt").exists());

    // Couper puis coller : déplacement, puis presse-papiers vidé.
    let dest2 = dir.path().join("dest2");
    std::fs::create_dir(&dest2)?;
    let f = file.clone();
    on_host(&host, move |_| fx_win::clipboard::set_files(&[f], true))??;
    fx_win::clipboard::paste(&dest2, silent)?;
    assert!(!file.exists() && dest2.join("presse-papiers.txt").exists());
    assert!(!fx_win::clipboard::has_files());
    Ok(())
}
