//! Utilitaires partagés par les benchmarks.

use std::path::PathBuf;

/// Tailles des jeux de fichiers générés.
pub const SIZES: [usize; 3] = [1_000, 10_000, 100_000];

/// Racine des jeux de fichiers : `FX_FIXTURES` ou `<workspace>/target/fixtures`.
pub fn fixtures_root() -> PathBuf {
    std::env::var_os("FX_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("target")
                .join("fixtures")
        })
}

/// Dossier contenant `n` entrées.
pub fn fixture(n: usize) -> PathBuf {
    fixtures_root().join(format!("n{n}"))
}

const EXTS: [&str; 12] = [
    "txt", "pdf", "docx", "xlsx", "jpg", "png", "rs", "json", "zip", "mp3", "exe", "lnk",
];
const WORDS: [&str; 10] = [
    "Rapport",
    "photo",
    "Facture",
    "notes",
    "Projet",
    "été",
    "copie",
    "Archive",
    "données",
    "Présentation",
];

/// Nom de la i-ème entrée d'un jeu de `n` entrées ; 2 % de dossiers.
pub fn fixture_name(i: usize, n: usize) -> (String, bool) {
    let h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let word = WORDS[(h >> 7) as usize % WORDS.len()];
    if i < n / 50 {
        (format!("{word} {i}"), true)
    } else {
        let ext = EXTS[(h >> 13) as usize % EXTS.len()];
        (format!("{word} {} - {i:06}.{ext}", (h >> 20) % 1000), false)
    }
}
