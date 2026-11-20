//! Fonctions dépendant des paramètres régionaux de l'utilisateur : dates, clés de tri.

use std::cmp::Ordering;

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::Globalization::{
    DATE_SHORTDATE, GetDateFormatEx, GetTimeFormatEx, LCMAP_SORTKEY, LCMapStringEx, NORM_IGNORECASE,
    SORT_DIGITSASNUMBERS, TIME_NOSECONDS,
};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::Shell::StrCmpLogicalW;
use windows::core::PCWSTR;

/// Formate un FILETIME UTC en date et heure locales courtes (« 20/11/2026 14:05 » en fr-FR).
pub fn format_filetime(ft: i64) -> Option<String> {
    if ft <= 0 {
        return None;
    }
    let ft = FILETIME {
        dwLowDateTime: ft as u32,
        dwHighDateTime: (ft as u64 >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    // SAFETY: pointeurs vers des structures locales initialisées, vivantes pendant les appels.
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    let mut buf = [0u16; 128];
    // SAFETY: `local` est valide ; le tampon de sortie est une tranche dont la longueur est
    // transmise par le wrapper ; un nom de locale nul désigne LOCALE_NAME_USER_DEFAULT.
    let n = unsafe {
        GetDateFormatEx(
            PCWSTR::null(),
            DATE_SHORTDATE,
            Some(&local),
            PCWSTR::null(),
            Some(&mut buf),
            PCWSTR::null(),
        )
    };
    if n <= 0 {
        return None;
    }
    let mut out = String::from_utf16_lossy(&buf[..n as usize - 1]);
    out.push(' ');
    // SAFETY: idem.
    let n = unsafe {
        GetTimeFormatEx(
            PCWSTR::null(),
            TIME_NOSECONDS,
            Some(&local),
            PCWSTR::null(),
            Some(&mut buf),
        )
    };
    if n > 0 {
        out.push_str(&String::from_utf16_lossy(&buf[..n as usize - 1]));
    }
    Some(out)
}

thread_local! {
    static KEY_BUF: std::cell::RefCell<Vec<u16>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Ajoute à `out` la clé de tri « naturel » de `text` pour la locale de l'utilisateur :
/// insensible à la casse, nombres comparés par valeur (`SORT_DIGITSASNUMBERS`).
///
/// Deux clés se comparent octet par octet (`memcmp`). Renvoie `false` si la clé n'a pu être
/// calculée (dans ce cas `out` est inchangé).
pub fn natural_sort_key(text: &[u16], out: &mut Vec<u8>) -> bool {
    if text.is_empty() {
        out.push(0);
        return true;
    }
    let flags = LCMAP_SORTKEY | NORM_IGNORECASE.0 | SORT_DIGITSASNUMBERS.0;
    KEY_BUF.with_borrow_mut(|buf| {
        // Avec LCMAP_SORTKEY, la taille de destination est comptée en **octets**, alors que le
        // wrapper transmet la longueur de la tranche u16 : on annonce donc la moitié de la
        // capacité réelle, ce qui reste sûr.
        let mut want_bytes = text.len() * 6 + 64;
        loop {
            if buf.len() < want_bytes {
                buf.resize(want_bytes, 0);
            }
            // SAFETY: source et destination sont des tranches valides ; la destination annoncée
            // (`want_bytes` octets) est plus petite que sa taille réelle (2 × `want_bytes`).
            let n = unsafe {
                LCMapStringEx(
                    PCWSTR::null(),
                    flags,
                    text,
                    Some(&mut buf[..want_bytes]),
                    None,
                    None,
                    Default::default(),
                )
            };
            if n > 0 {
                // SAFETY: l'API a écrit `n` octets au début du tampon (n ≤ want_bytes ≤ taille réelle).
                let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), n as usize) };
                out.extend_from_slice(bytes);
                return true;
            }
            // Tampon insuffisant : on demande la taille exacte.
            // SAFETY: appel de mesure sans destination.
            let need =
                unsafe { LCMapStringEx(PCWSTR::null(), flags, text, None, None, None, Default::default()) };
            if need <= 0 || need as usize <= want_bytes {
                return false;
            }
            want_bytes = need as usize;
        }
    })
}

/// Comparaison de l'Explorateur Windows (`StrCmpLogicalW`). Lente : réservée aux tests de
/// conformité de notre tri.
pub fn str_cmp_logical(a: &[u16], b: &[u16]) -> Ordering {
    let za: Vec<u16> = a.iter().copied().chain(std::iter::once(0)).collect();
    let zb: Vec<u16> = b.iter().copied().chain(std::iter::once(0)).collect();
    // SAFETY: deux chaînes terminées par un nul, vivantes pendant l'appel.
    let r = unsafe { StrCmpLogicalW(PCWSTR(za.as_ptr()), PCWSTR(zb.as_ptr())) };
    r.cmp(&0)
}
