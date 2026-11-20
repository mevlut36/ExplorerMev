//! « Remplacer Win+E » : écrit sous une clé de test de HKCU (jamais sous Software\Classes),
//! supprimée à la fin.

use std::path::Path;

use fx_win::win_e::{WinEError, WinEKey, WinEState};

#[test]
fn enable_disable_roundtrip_and_respects_other_programs() -> Result<(), Box<dyn std::error::Error>> {
    let key = WinEKey::with_base(format!(r"Software\ExplorerMev-test-{}", std::process::id()));
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(key.state()?, WinEState::Default);
        // Désactiver sans avoir activé : rien à faire.
        assert!(!key.disable()?);

        let exe = Path::new(r"C:\Program Files\ExplorerMev\explorermev.exe");
        key.enable(exe)?;
        assert_eq!(key.state()?, WinEState::ExplorerMev(exe.to_path_buf()));
        // Réactiver (mise à jour du chemin) est permis.
        key.enable(exe)?;
        assert!(key.disable()?);
        assert_eq!(key.state()?, WinEState::Default);
        Ok(())
    })();
    key.delete_base()?;
    result?;

    // Win+E déjà pris par un autre programme : refus, et la désactivation n'y touche pas.
    let other = WinEKey::with_base(format!(r"Software\ExplorerMev-test-autre-{}", std::process::id()));
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        other.enable(Path::new(r"C:\Autre\explorer2.exe"))?;
        // On simule un autre programme : retrait du marqueur d'ExplorerMev.
        let marker = format!(
            r"HKCU\Software\ExplorerMev-test-autre-{}\CLSID\{{52205fd8-5dfb-447d-801a-d0b52f2e83e1}}",
            std::process::id()
        );
        let status = std::process::Command::new("reg")
            .args(["delete", &marker, "/v", "ExplorerMev", "/f"])
            .output()?
            .status;
        assert!(status.success());
        assert!(matches!(other.state()?, WinEState::Other(_)));
        assert!(matches!(
            other.enable(Path::new(r"C:\ExplorerMev\explorermev.exe")),
            Err(WinEError::TakenByOther(_))
        ));
        assert!(!other.disable()?);
        assert!(matches!(other.state()?, WinEState::Other(_)));
        Ok(())
    })();
    other.delete_base()?;
    result
}
