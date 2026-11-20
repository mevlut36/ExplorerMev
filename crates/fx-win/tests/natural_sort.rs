//! Conformité de la clé de tri naturel (`LCMapStringEx` + `SORT_DIGITSASNUMBERS`) avec
//! `StrCmpLogicalW`, la comparaison de l'Explorateur.

use std::cmp::Ordering;

use fx_win::locale::{natural_sort_key, str_cmp_logical};

fn key(s: &[u16]) -> Vec<u8> {
    let mut k = Vec::new();
    assert!(natural_sort_key(s, &mut k));
    k
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn by_key(names: &[Vec<u16>]) -> Vec<Vec<u16>> {
    let mut v = names.to_vec();
    v.sort_by_cached_key(|n| key(n));
    v
}

fn by_explorer(names: &[Vec<u16>]) -> Vec<Vec<u16>> {
    let mut v = names.to_vec();
    v.sort_by(|a, b| str_cmp_logical(a, b));
    v
}

fn show(v: &[Vec<u16>]) -> Vec<String> {
    v.iter().map(|n| String::from_utf16_lossy(n)).collect()
}

#[test]
fn numbers_are_compared_by_value() {
    let names: Vec<_> = [
        "fichier10.txt",
        "fichier2.txt",
        "fichier1.txt",
        "fichier100.txt",
        "fichier02.txt",
    ]
    .map(w)
    .to_vec();
    let sorted = show(&by_key(&names));
    let pos = |s: &str| sorted.iter().position(|x| x == s);
    assert!(pos("fichier1.txt") < pos("fichier2.txt"));
    assert!(pos("fichier2.txt") < pos("fichier10.txt"));
    assert!(pos("fichier10.txt") < pos("fichier100.txt"));
}

#[test]
fn curated_cases_match_explorer() {
    let names: Vec<Vec<u16>> = [
        "a",
        "B",
        "c",
        "été",
        "ete",
        "Eté",
        "zoo",
        "Zèbre",
        "écran",
        "1",
        "2",
        "10",
        "01",
        "001",
        "a1",
        "a01",
        "a2",
        "a10",
        "a10b",
        "a10a",
        "a 1",
        "a_1",
        "a-1",
        "a.1",
        "(1)",
        "[1]",
        "_x",
        "~x",
        "#x",
        "x (2)",
        "x (10)",
        "x (1)",
        "Rapport 2024-03-01.pdf",
        "Rapport 2024-3-1.pdf",
        "Œuvre",
        "oeuvre",
        "ça",
        "ca",
        "日本",
        "ÉCOLE",
        "école",
    ]
    .map(w)
    .to_vec();
    let ours = by_key(&names);
    let theirs = by_explorer(&names);
    // Les égalités de StrCmpLogicalW (ex. « a » / « A ») peuvent être ordonnées différemment
    // par un tri stable : on compare donc par paires adjacentes.
    for pair in ours.windows(2) {
        assert_ne!(
            str_cmp_logical(&pair[0], &pair[1]),
            Ordering::Greater,
            "désaccord : {:?} avant {:?}\nnous : {:?}\nExplorateur : {:?}",
            String::from_utf16_lossy(&pair[0]),
            String::from_utf16_lossy(&pair[1]),
            show(&ours),
            show(&theirs)
        );
    }
}

/// Mesure le taux d'accord sur un corpus pseudo-aléatoire (affiché avec `--nocapture`).
#[test]
fn random_corpus_agreement() {
    let alphabet: Vec<char> = "aAbBeéEÉ zZ0123456789_-.()çœ😀".chars().collect();
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut pairs = 0u32;
    let mut disagree = Vec::new();
    for _ in 0..20_000 {
        let mut make = || {
            let len = 1 + (next() % 8) as usize;
            (0..len)
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect::<String>()
        };
        let (a, b) = (make(), make());
        let (wa, wb) = (w(&a), w(&b));
        let ours = key(&wa).cmp(&key(&wb));
        let theirs = str_cmp_logical(&wa, &wb);
        pairs += 1;
        if theirs != Ordering::Equal && ours != theirs {
            disagree.push((a, b, ours, theirs));
        }
    }
    let rate = disagree.len() as f64 / pairs as f64 * 100.0;
    println!("désaccords : {} / {pairs} ({rate:.3} %)", disagree.len());
    for d in disagree.iter().take(15) {
        println!("  {:?} vs {:?} : nous {:?}, Explorateur {:?}", d.0, d.1, d.2, d.3);
    }
    assert!(rate < 1.0, "trop de désaccords avec StrCmpLogicalW : {rate:.3} %");
}
