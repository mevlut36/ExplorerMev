//! Tri naturel, filtre et chargement complet d'un dossier (listing + types + clés + tri).

use std::hint::black_box;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use fx_bench::{SIZES, fixture};
use fx_core::collate::Collator;
use fx_core::filter::Filter;
use fx_core::folder::Folder;
use fx_core::sort::{NameKeys, SortColumn, SortSpec, sorted_order_with_keys};
use fx_core::source::DirSource;
use fx_core::sys::{WinCollator, WinSource};

/// Chargement synchrone équivalent à celui du `Loader` (sans thread ni lots).
fn load(path: &Path, src: &WinSource, collator: &dyn Collator) -> (Folder, NameKeys, Vec<u32>) {
    let mut folder = Folder::with_capacity(path.to_path_buf(), 1024);
    let mut types = std::collections::HashMap::<(Vec<u16>, bool), u32>::new();
    let r = src.list(path, &|| false, &mut |e| {
        let is_dir = e.attributes & 0x10 != 0;
        let key = (
            if is_dir {
                Vec::new()
            } else {
                fx_core::folder::extension_key(e.name)
            },
            is_dir,
        );
        let id = match types.get(&key) {
            Some(&id) => id,
            None => {
                let label: Arc<str> = Arc::from(src.type_name(e.name, is_dir).unwrap_or_default());
                let id = folder.intern_type(label);
                types.insert(key, id);
                id
            }
        };
        folder.push(e, id);
    });
    assert!(r.is_ok());
    let keys = NameKeys::build(&folder, collator);
    let order = sorted_order_with_keys(&folder, SortSpec::default(), &keys, collator);
    (folder, keys, order)
}

fn sort(c: &mut Criterion) {
    let _com = fx_win::com::ComApartment::sta();
    let src = WinSource::default();
    let collator = WinCollator;
    let mut g = c.benchmark_group("dossier");
    g.warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(4))
        .sample_size(20);
    for n in SIZES {
        let dir = fixture(n);
        if !dir.exists() {
            eprintln!("{} absent : lancer gen-fixtures", dir.display());
            continue;
        }
        g.bench_with_input(BenchmarkId::new("chargement-complet", n), &dir, |b, dir| {
            b.iter(|| black_box(load(dir, &src, &collator).2.len()))
        });
        let (folder, keys, order) = load(&dir, &src, &collator);
        g.bench_with_input(BenchmarkId::new("cles-de-tri", n), &folder, |b, f| {
            b.iter(|| black_box(NameKeys::build(f, &collator)))
        });
        for (label, column) in [("tri-nom", SortColumn::Name), ("tri-taille", SortColumn::Size)] {
            let spec = SortSpec {
                column,
                descending: false,
            };
            g.bench_with_input(BenchmarkId::new(label, n), &folder, |b, f| {
                b.iter(|| black_box(sorted_order_with_keys(f, spec, &keys, &collator)))
            });
        }
        let filter = Filter::new("rapport 4");
        g.bench_with_input(BenchmarkId::new("filtre", n), &folder, |b, f| {
            b.iter(|| black_box(filter.apply(f, &order)))
        });
    }
    g.finish();
}

criterion_group!(benches, sort);
criterion_main!(benches);
