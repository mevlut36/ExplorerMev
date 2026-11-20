//! Vérification sur le dossier OneDrive réel de la machine (ignoré par défaut : dépend du
//! poste). N'affiche que des compteurs, jamais de noms de fichiers.

use std::path::{Path, PathBuf};

use fx_core::folder::{CloudState, Folder};
use fx_core::source::DirSource;
use fx_core::sys::WinSource;

fn walk(dir: &Path, depth: usize, src: &WinSource, counts: &mut [usize; 4]) {
    let mut folder = Folder::new(dir.to_path_buf());
    if src.list(dir, &|| false, &mut |e| folder.push(e, 0)).is_err() {
        return;
    }
    for i in 0..folder.len() as u32 {
        let Some(e) = folder.entry(i) else { continue };
        if e.is_dir() {
            if depth > 0 {
                walk(&folder.child_path(i), depth - 1, src, counts);
            }
            continue;
        }
        let k = match e.cloud_state() {
            CloudState::None => 0,
            CloudState::OnlineOnly => 1,
            CloudState::Local => 2,
            CloudState::Pinned => 3,
        };
        counts[k] += 1;
    }
}

#[test]
#[ignore = "dépend du dossier OneDrive de la machine"]
fn onedrive_states_are_recognized() {
    let Some(root) = std::env::var_os("OneDrive").map(PathBuf::from) else {
        println!("pas de OneDrive");
        return;
    };
    let mut counts = [0usize; 4];
    walk(&root, 3, &WinSource::default(), &mut counts);
    println!(
        "aucun={} en_ligne={} local={} toujours_conservé={}",
        counts[0], counts[1], counts[2], counts[3]
    );
    assert!(
        counts[1] + counts[2] + counts[3] > 0,
        "aucun élément OneDrive reconnu"
    );
}
