//! Sonde : durée d'un parcours complet d'un volume sans droits d'administration (comptes seuls).
fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| r"C:\".into());
    let source = fx_core::sys::WinSource::default();
    let t = std::time::Instant::now();
    let p = fx_core::size::compute(&source, std::path::Path::new(&root));
    println!(
        "{} fichiers, {} dossiers, {} dossiers illisibles, en {:.1} s",
        p.files.load(std::sync::atomic::Ordering::Relaxed),
        p.dirs.load(std::sync::atomic::Ordering::Relaxed),
        p.errors.load(std::sync::atomic::Ordering::Relaxed),
        t.elapsed().as_secs_f64()
    );
}
