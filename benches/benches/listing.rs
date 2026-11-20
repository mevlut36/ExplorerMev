//! Compare les méthodes de listing (ADR 0002), cache chaud.
//! Prérequis : `cargo run --release -p fx-bench --bin gen-fixtures`.

use std::hint::black_box;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use fx_bench::{SIZES, fixture};
use fx_win::listing::{ListMethod, list_dir_exact};

fn listing(c: &mut Criterion) {
    let mut g = c.benchmark_group("listing");
    g.warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(4))
        .sample_size(20);
    let variants: [(&str, Option<(ListMethod, usize)>); 6] = [
        ("idextd-64k", Some((ListMethod::IdExtdDirectoryInfo, 64 << 10))),
        ("idextd-256k", Some((ListMethod::IdExtdDirectoryInfo, 256 << 10))),
        ("idextd-1m", Some((ListMethod::IdExtdDirectoryInfo, 1 << 20))),
        ("full-64k", Some((ListMethod::FullDirectoryInfo, 64 << 10))),
        ("findfirstex", Some((ListMethod::FindFirstFileEx, 0))),
        ("std-read_dir", None),
    ];
    for n in SIZES {
        let dir = fixture(n);
        if !dir.exists() {
            eprintln!("{} absent : lancer gen-fixtures", dir.display());
            continue;
        }
        g.throughput(Throughput::Elements(n as u64));
        for (name, v) in variants {
            g.bench_with_input(BenchmarkId::new(name, n), &dir, |b, dir| {
                b.iter(|| {
                    let mut count = 0usize;
                    let mut bytes = 0u64;
                    match v {
                        Some((m, buf)) => {
                            let r = list_dir_exact(dir, m, buf, &mut |e| {
                                count += 1;
                                bytes += e.size + e.name.len() as u64;
                            });
                            assert!(r.is_ok());
                        }
                        None => {
                            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                                count += 1;
                                // Taille via les métadonnées de l'entrée (fournies par l'énumération).
                                bytes +=
                                    e.metadata().map(|m| m.len()).unwrap_or(0) + e.file_name().len() as u64;
                            }
                        }
                    }
                    assert_eq!(count, n);
                    black_box(bytes)
                })
            });
        }
    }
    g.finish();
}

criterion_group!(benches, listing);
criterion_main!(benches);
