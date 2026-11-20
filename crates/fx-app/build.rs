fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/app.manifest");
    println!("cargo:rerun-if-changed=res/app.ico");
    // Version de l'exécutable : celle de Cargo.toml (le paquet et le manifeste winget la
    // reprennent, rien n'est saisi à la main à deux endroits).
    let num = |name: &str| std::env::var(name).unwrap_or_else(|_| "0".into());
    let macros = [
        format!("VER_MAJOR={}", num("CARGO_PKG_VERSION_MAJOR")),
        format!("VER_MINOR={}", num("CARGO_PKG_VERSION_MINOR")),
        format!("VER_PATCH={}", num("CARGO_PKG_VERSION_PATCH")),
        format!(
            "VER_STR=\"{}\"",
            std::env::var("CARGO_PKG_VERSION").unwrap_or_default()
        ),
    ];
    // Le manifeste est indispensable (DPI, chemins longs, Common Controls v6) : on échoue s'il
    // ne peut pas être intégré.
    if let Err(e) = embed_resource::compile("res/app.rc", macros).manifest_required() {
        eprintln!("intégration des ressources Windows impossible : {e}");
        std::process::exit(1);
    }
}
