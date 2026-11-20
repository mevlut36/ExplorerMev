//! Génère les jeux de fichiers de test (1 000, 10 000 et 100 000 entrées).
//!
//! Usage : `cargo run --release -p fx-bench --bin gen-fixtures [-- <racine>]`
//! Les fichiers sont vides (seuls les noms et métadonnées comptent pour le listing). Un jeu déjà
//! complet n'est pas regénéré.

use std::path::PathBuf;
use std::time::Instant;

use fx_bench::{SIZES, fixture_name, fixtures_root};

fn main() -> std::io::Result<()> {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(fixtures_root);
    for n in SIZES {
        let dir = root.join(format!("n{n}"));
        let existing = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
        if existing == n {
            println!("{} : déjà présent ({n} entrées)", dir.display());
            continue;
        }
        if existing > 0 {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        let t = Instant::now();
        for i in 0..n {
            let (name, is_dir) = fixture_name(i, n);
            let p = dir.join(name);
            if is_dir {
                std::fs::create_dir(&p)?;
            } else {
                std::fs::File::create(&p)?;
            }
        }
        println!(
            "{} : {n} entrées créées en {:.1} s",
            dir.display(),
            t.elapsed().as_secs_f64()
        );
    }
    Ok(())
}
