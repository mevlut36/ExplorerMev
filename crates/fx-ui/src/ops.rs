//! Exécution des opérations sur les fichiers : chacune sur son propre thread OLE (STA), car
//! `IFileOperation` peut durer et afficher des dialogues. Le thread d'interface n'attend jamais.

use std::path::PathBuf;

use fx_win::fileop::{FileOp, OpOptions, OpOutcome, Source, copy_or_move, perform};

/// Ce qu'il faut faire.
pub enum Job {
    Op(FileOp),
    Paste {
        dest: PathBuf,
    },
    Drop {
        items: Vec<PathBuf>,
        dest: PathBuf,
        is_move: bool,
    },
}

/// Lance `job` ; `done` est appelé depuis le thread de l'opération.
pub fn spawn(job: Job, owner: isize, done: impl FnOnce(Result<OpOutcome, String>) + Send + 'static) {
    let spawned = std::thread::Builder::new()
        .name("fx-fileop".into())
        .spawn(move || {
            let _ole = match fx_win::com::OleApartment::init() {
                Ok(o) => o,
                Err(e) => {
                    done(Err(format!("initialisation OLE impossible : {e}")));
                    return;
                }
            };
            let opts = OpOptions { owner, silent: false };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &job {
                Job::Op(op) => perform(op, opts),
                Job::Paste { dest } => fx_win::clipboard::paste(dest, opts).map(Option::unwrap_or_default),
                Job::Drop { items, dest, is_move } => {
                    copy_or_move(Source::Paths(items), dest, *is_move, opts)
                }
            }));
            done(match result {
                Ok(Ok(out)) => Ok(out),
                Ok(Err(e)) => Err(e.message()),
                Err(_) => Err("erreur interne pendant l'opération".into()),
            });
        });
    if let Err(e) = spawned {
        tracing::error!("thread d'opération non démarré : {e}");
    }
}
