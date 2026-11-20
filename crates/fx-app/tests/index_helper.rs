//! Assistant d'indexation lancé **sans** élévation : tout le chemin (tube réservé à
//! l'utilisateur, contrôle du pid du client, protocole) est exercé ; la MFT étant illisible
//! sans droits, l'assistant doit le signaler par une trame d'erreur puis s'arrêter.

use std::path::Path;
use std::time::Duration;

use fx_core::mft_proto::{Frame, read_frame};
use fx_win::pipe::{PipeServer, launch_plain, random_pipe_name};

#[test]
fn unelevated_helper_reports_access_denied_and_exits() -> Result<(), Box<dyn std::error::Error>> {
    let exe = Path::new(env!("CARGO_BIN_EXE_windir"));
    let name = random_pipe_name();
    let server = PipeServer::create(&name)?;
    let child = launch_plain(exe, &format!("--index-helper {name} {} C", std::process::id()))?;
    server.accept(child.pid, child.handle(), Duration::from_secs(20))?;
    match read_frame(&mut &server)? {
        Some(Frame::Error { letter: 'C', message }) => {
            assert!(!message.is_empty());
            // Plus de volume lisible : l'assistant s'arrête et ferme le tube.
            assert_eq!(read_frame(&mut &server)?, None);
            for _ in 0..50 {
                if child.exited() {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err("l'assistant ne s'est pas arrêté".into())
        }
        // Session élevée (machine d'intégration) : la MFT est lisible, c'est aussi correct.
        Some(Frame::Begin { letter: 'C', .. }) => Ok(()),
        other => Err(format!("trame inattendue : {other:?}").into()),
    }
}

#[test]
fn helper_rejects_bad_arguments() -> Result<(), Box<dyn std::error::Error>> {
    let exe = env!("CARGO_BIN_EXE_windir");
    for args in [
        vec!["--index-helper"],
        vec!["--index-helper", r"\\.\pipe\autre", "1", "C"],
        vec!["--index-helper", &random_pipe_name(), "1", "c"],
    ] {
        let status = std::process::Command::new(exe).args(&args).status()?;
        assert_eq!(status.code(), Some(2), "{args:?}");
    }
    Ok(())
}
