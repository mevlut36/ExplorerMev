fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    if let Err(e) = slint_build::compile_with_config("ui/app.slint", config) {
        eprintln!("échec de compilation Slint : {e}");
        std::process::exit(1);
    }
}
