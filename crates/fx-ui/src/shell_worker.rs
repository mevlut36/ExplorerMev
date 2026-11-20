//! Thread Shell dédié (COM STA) : les appels Shell bloquants n'ont jamais lieu sur le thread
//! d'interface.

use std::path::PathBuf;

use crossbeam_channel::{Sender, unbounded};

enum Job {
    Open { path: PathBuf, owner: Option<isize> },
}

pub struct ShellWorker {
    tx: Option<Sender<Job>>,
}

impl ShellWorker {
    /// `on_error` est appelé depuis le thread Shell avec un message destiné à l'utilisateur.
    pub fn start(on_error: impl Fn(String) + Send + 'static) -> Self {
        let (tx, rx) = unbounded::<Job>();
        let spawned = std::thread::Builder::new()
            .name("fx-shell".into())
            .spawn(move || {
                let _com = match fx_win::com::ComApartment::sta() {
                    Ok(c) => Some(c),
                    Err(e) => {
                        tracing::error!("COM indisponible pour le thread Shell : {e}");
                        None
                    }
                };
                for job in rx {
                    match job {
                        Job::Open { path, owner } => {
                            if let Err(e) = fx_win::shell::open(&path, owner) {
                                tracing::warn!(path = %path.display(), "ouverture impossible : {e}");
                                on_error(format!(
                                    "Impossible d'ouvrir « {} » : {}",
                                    path.display(),
                                    e.message()
                                ));
                            }
                        }
                    }
                }
            });
        match spawned {
            Ok(_) => Self { tx: Some(tx) },
            Err(e) => {
                tracing::error!("thread Shell non démarré : {e}");
                Self { tx: None }
            }
        }
    }

    pub fn open(&self, path: PathBuf, owner: Option<isize>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Job::Open { path, owner });
        }
    }
}
