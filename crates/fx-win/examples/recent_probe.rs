//! Sonde : combien d'éléments récents sont lisibles ? (nombres seulement, aucun nom)
fn main() {
    let _com = fx_win::com::ComApartment::sta().ok();
    let t = std::time::Instant::now();
    let items = fx_win::recent::recent_items(40);
    let dirs = items.iter().filter(|i| i.is_dir).count();
    let net = items
        .iter()
        .filter(|i| i.target.to_string_lossy().starts_with(r"\\"))
        .count();
    println!(
        "{} éléments récents ({} dossiers, {} réseau) en {:?}",
        items.len(),
        dirs,
        net,
        t.elapsed()
    );
}
