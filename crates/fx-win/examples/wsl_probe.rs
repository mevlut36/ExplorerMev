//! Sonde : que renvoie le listing sur les chemins WSL ? (nombres seulement)
//! Usage : wsl_probe <distribution>
use std::path::Path;

fn probe(label: &str, path: &str) {
    let mut n = 0usize;
    let mut dirs = 0usize;
    let t = std::time::Instant::now();
    let r = fx_win::listing::list_dir(
        Path::new(path),
        &fx_win::listing::ListOptions::default(),
        &|| false,
        &mut |e| {
            n += 1;
            if e.attributes & 0x10 != 0 {
                dirs += 1;
            }
        },
    );
    match r {
        Ok(m) => println!(
            "{label:<26} OK via {m:?} : {n} entrées ({dirs} dossiers) en {:?}",
            t.elapsed()
        ),
        Err(e) => println!("{label:<26} ÉCHEC : {e:?} (après {:?})", t.elapsed()),
    }
}

fn main() {
    let Some(d) = std::env::args().nth(1) else {
        println!("usage : wsl_probe <distribution>");
        return;
    };
    probe("distribution", &format!(r"\\wsl.localhost\{d}"));
    probe("distribution (2e fois)", &format!(r"\\wsl.localhost\{d}"));
    probe("distribution\\home", &format!(r"\\wsl.localhost\{d}\home"));
    probe("distribution\\etc", &format!(r"\\wsl.localhost\{d}\etc"));
    probe("alias \\\\wsl$", &format!(r"\\wsl$\{d}"));
    for m in [
        fx_win::listing::ListMethod::IdExtdDirectoryInfo,
        fx_win::listing::ListMethod::FullDirectoryInfo,
        fx_win::listing::ListMethod::FindFirstFileEx,
    ] {
        let mut n = 0usize;
        let r = fx_win::listing::list_dir_exact(
            Path::new(&format!(r"\\wsl.localhost\{d}\etc")),
            m,
            64 * 1024,
            &mut |_| n += 1,
        );
        println!(
            "méthode {m:?} sur \\etc : {}",
            match r {
                Ok(()) => format!("OK, {n} entrées"),
                Err(e) => format!("ÉCHEC {e:?}"),
            }
        );
    }
}
