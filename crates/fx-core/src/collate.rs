//! Clés de tri « naturel » (nombres comparés par valeur, casse ignorée).

/// Produit des clés de tri comparables octet par octet.
pub trait Collator: Send + Sync {
    /// Ajoute à `out` la clé de `name`.
    fn key(&self, name: &[u16], out: &mut Vec<u8>);
}

/// Collation portable, sans données de locale : minuscules Unicode, chiffres ASCII comparés par
/// valeur. Sert aux tests et de repli ; sous Windows on utilise la collation système
/// (identique à l'Explorateur, voir `sys::WinCollator`).
///
/// Encodage : les segments de texte sont écrits en UTF-8 minuscule (aucun octet nul) ; un
/// segment de chiffres devient `'0'`, sa longueur significative (u16 gros-boutiste), ses chiffres
/// significatifs, puis le nombre de zéros initiaux. Comme `'0'` (0x30) est après l'espace et la
/// plupart de la ponctuation, et avant les lettres, les nombres se placent à peu près comme
/// dans l'Explorateur (`_` fait exception : l'Explorateur le place avant les chiffres).
#[derive(Clone, Copy, Debug, Default)]
pub struct PortableCollator;

impl Collator for PortableCollator {
    fn key(&self, name: &[u16], out: &mut Vec<u8>) {
        let chars: Vec<char> = char::decode_utf16(name.iter().copied())
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i].is_ascii_digit() {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let digits = &chars[start..i];
                let zeros = digits
                    .iter()
                    .take_while(|&&c| c == '0')
                    .count()
                    .min(digits.len() - 1);
                let significant = &digits[zeros..];
                out.push(b'0');
                out.extend_from_slice(&(significant.len().min(u16::MAX as usize) as u16).to_be_bytes());
                out.extend(significant.iter().map(|&c| c as u8));
                out.push(zeros.min(255) as u8);
            } else {
                let mut buf = [0u8; 4];
                for lc in chars[i].to_lowercase() {
                    out.extend_from_slice(lc.encode_utf8(&mut buf).as_bytes());
                }
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(names: &[&str]) -> Vec<String> {
        let c = PortableCollator;
        let mut v: Vec<(Vec<u8>, String)> = names
            .iter()
            .map(|n| {
                let mut k = Vec::new();
                c.key(&n.encode_utf16().collect::<Vec<_>>(), &mut k);
                (k, n.to_string())
            })
            .collect();
        v.sort();
        v.into_iter().map(|(_, n)| n).collect()
    }

    #[test]
    fn numbers_by_value() {
        assert_eq!(sorted(&["f10", "f2", "f1", "f100"]), ["f1", "f2", "f10", "f100"]);
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(sorted(&["b", "A", "c"]), ["A", "b", "c"]);
    }

    #[test]
    fn digits_before_letters_after_punctuation() {
        assert_eq!(sorted(&["a", "1", "_", " x"]), [" x", "1", "_", "a"]);
    }

    #[test]
    fn leading_zeros_break_ties_only() {
        assert_eq!(sorted(&["x02", "x1", "x2", "x10"]), ["x1", "x2", "x02", "x10"]);
    }

    #[test]
    fn huge_numbers_do_not_overflow() {
        assert_eq!(
            sorted(&["99999999999999999999999", "100000000000000000000000", "5"]),
            ["5", "99999999999999999999999", "100000000000000000000000"]
        );
    }

    #[test]
    fn prefix_sorts_first() {
        assert_eq!(sorted(&["ab", "a1", "a"]), ["a", "a1", "ab"]);
    }
}
