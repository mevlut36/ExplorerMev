//! Formatage indépendant de la plateforme.

/// Taille à la manière de l'Explorateur en vue détails : kilo-octets arrondis au supérieur,
/// séparateur de milliers (espace fine insécable). « 0 Ko » pour un fichier vide.
pub fn size_kb(size: u64) -> String {
    let kb = size.div_ceil(1024);
    format!("{} Ko", group_thousands(kb))
}

pub fn group_thousands(v: u64) -> String {
    let s = v.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3 * 3);
    for (k, c) in s.chars().enumerate() {
        if k > 0 && (s.len() - k).is_multiple_of(3) {
            out.push('\u{202F}');
        }
        out.push(c);
    }
    out
}

/// « 1 élément », « 12 345 éléments ».
pub fn item_count(n: usize) -> String {
    if n == 1 {
        "1 élément".into()
    } else {
        format!("{} éléments", group_thousands(n as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size_kb(0), "0 Ko");
        assert_eq!(size_kb(1), "1 Ko");
        assert_eq!(size_kb(1024), "1 Ko");
        assert_eq!(size_kb(1025), "2 Ko");
        assert_eq!(size_kb(1_048_576_000), "1\u{202F}024\u{202F}000 Ko");
    }

    #[test]
    fn counts() {
        assert_eq!(item_count(1), "1 élément");
        assert_eq!(item_count(0), "0 éléments");
        assert_eq!(item_count(100_000), "100\u{202F}000 éléments");
    }
}
