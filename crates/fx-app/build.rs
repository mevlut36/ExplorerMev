fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/app.manifest");
    // Le manifeste est indispensable (DPI, chemins longs, Common Controls v6) : on échoue s'il
    // ne peut pas être intégré.
    if let Err(e) = embed_resource::compile("res/app.rc", embed_resource::NONE).manifest_required() {
        eprintln!("intégration des ressources Windows impossible : {e}");
        std::process::exit(1);
    }
}
