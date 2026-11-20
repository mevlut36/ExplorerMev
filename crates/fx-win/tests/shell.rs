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
