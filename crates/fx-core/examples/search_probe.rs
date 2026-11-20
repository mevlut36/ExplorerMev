//! Mesure de la recherche sur un volume réel : construction de l'index par parcours (sans
//! droits), mémoire, cache disque, puis durée de requêtes tapées lettre par lettre.
//! N'affiche que des nombres (aucun nom de fichier).
use std::sync::atomic::Ordering;
use std::time::Instant;

use fx_core::search::{Query, VolumeIndex, search};
use fx_core::search_walk::{WalkProgress, build};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let letter = std::env::args()
        .nth(1)
        .and_then(|s| s.chars().next())
        .unwrap_or('C');
    let cache = std::env::args().nth(2);
    let p = WalkProgress::default();
    let t = Instant::now();
    let mut ix = build(letter, &p)?;
    println!(
        "index {letter}: {} éléments en {:.1} s ({} dossiers illisibles), mémoire {:.1} Mo",
        ix.len(),
        t.elapsed().as_secs_f64(),
        p.unreadable.load(Ordering::Relaxed),
        ix.memory_bytes() as f64 / 1_048_576.0
    );
    if let Some(path) = cache {
        let t = Instant::now();
        let mut f = std::io::BufWriter::new(std::fs::File::create(&path)?);
        ix.write_to(&mut f)?;
        drop(f);
        let size = std::fs::metadata(&path)?.len();
        let w = t.elapsed();
        let t = Instant::now();
        let back = VolumeIndex::read_from(&mut std::io::BufReader::new(std::fs::File::open(&path)?))?;
        println!(
            "cache : {:.1} Mo, écrit en {:.0} ms, relu et vérifié en {:.0} ms ({} éléments)",
            size as f64 / 1_048_576.0,
            w.as_secs_f64() * 1000.0,
            t.elapsed().as_secs_f64() * 1000.0,
            back.len()
        );
        std::fs::remove_file(&path)?;
    }
    let typed = [
        "w", "wi", "win", "wind", "windo", "window", "windows", "e", "ex", "exe", ".dll", "system32", "zzqx",
        "été", "a b c",
    ];
    let mut worst = 0f64;
    for text in typed {
        let q = Query::parse(text).ok_or("requête vide")?;
        let mut times = Vec::new();
        let mut total = 0;
        for _ in 0..7 {
            let t = Instant::now();
            let r = search(&[&ix], &q, 1000, &|| false).ok_or("annulée")?;
            // Chemins des résultats affichés (une page).
            let shown: usize = r
                .hits
                .iter()
                .take(40)
                .filter_map(|h| ix.path(h.item as usize))
                .count();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            std::hint::black_box(shown);
            total = r.total;
        }
        times.sort_by(f64::total_cmp);
        worst = worst.max(times[6]);
        println!(
            "« {text} » : {total} résultats, médiane {:.1} ms, max {:.1} ms",
            times[3], times[6]
        );
    }
    println!("pire durée : {worst:.1} ms");
    Ok(())
}
