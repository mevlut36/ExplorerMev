//! Index de recherche par nom, pour tout un volume.
//!
//! Chaque élément est désigné par son **numéro d'enregistrement MFT** (48 bits de poids faible
//! de la référence de fichier NTFS), qu'il vienne de la MFT (assistant élevé) ou d'un parcours
//! des dossiers (`FileIdExtdDirectoryInfo` donne la même référence). Le parent est lui aussi un
//! numéro d'enregistrement : les mises à jour (création, renommage, suppression) ne touchent
//! qu'un élément.
//!
//! Représentation compacte : 16 octets par élément, noms en UTF-8 à la suite dans un seul
//! tableau, et une table « enregistrement → élément ». Les éléments ne sont jamais déplacés :
//! un renommage marque l'ancien élément supprimé et en ajoute un nouveau ; le tableau des noms
//! reste donc dans l'ordre des éléments, ce que la recherche exploite. Un compactage récupère
//! la place quand plus d'un quart des éléments sont supprimés.
//!
//! Recherche : chaque mot doit apparaître dans le nom, sans tenir compte de la casse. Le mot le
//! plus long est cherché dans le tableau des noms par `memchr` (premier caractère, majuscule et
//! minuscule), en parallèle sur des tranches d'éléments ; les autres mots sont vérifiés sur le
//! nom trouvé. Classement : nom exact, puis début du nom, puis le reste ; noms courts d'abord.

use std::io::{self, Read, Write};

use rayon::prelude::*;

/// Enregistrement MFT du dossier racine d'un volume NTFS.
pub const ROOT_REC: u32 = 5;
const NONE: u32 = u32::MAX;
/// Profondeur maximale d'un chemin reconstruit (protection contre une boucle de parents).
const MAX_DEPTH: usize = 512;
/// Éléments par tranche de recherche parallèle.
const CHUNK: usize = 32 * 1024;

const ATTR_HIDDEN: u32 = 0x2;
const ATTR_SYSTEM: u32 = 0x4;
const ATTR_DIRECTORY: u32 = 0x10;

pub mod flag {
    pub const DIR: u16 = 1;
    pub const HIDDEN: u16 = 2;
    pub const SYSTEM: u16 = 4;
    /// Nom UTF-16 invalide (substitut isolé), stocké avec des caractères de remplacement.
    pub const LOSSY: u16 = 8;
    pub const DELETED: u16 = 0x8000;
}

/// Agrandit `v` pour `extra` éléments de plus, par pas d'1/16 (au moins 64 Kio) et non en
/// doublant : sur un index de plus de 100 Mo, la première insertion après un chargement
/// doublerait sinon la mémoire.
fn grow<T>(v: &mut Vec<T>, extra: usize) {
    if v.len() + extra > v.capacity() {
        let step = (v.len() / 16).max(65_536 / size_of::<T>().max(1));
        v.reserve_exact(extra.max(step));
    }
}

/// Numéro d'enregistrement MFT d'une référence de fichier NTFS (`None` s'il ne tient pas).
pub fn record_of(file_ref: u64) -> Option<u32> {
    let rec = file_ref & 0xFFFF_FFFF_FFFF;
    (rec < NONE as u64).then_some(rec as u32)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Item {
    rec: u32,
    parent: u32,
    off: u32,
    len: u16,
    flags: u16,
}

/// Origine et point de reprise de l'index (enregistrés avec lui).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexMeta {
    /// Numéro de série du volume : un cache d'un autre volume est ignoré.
    pub serial: u32,
    /// 0 : parcours des dossiers ; 1 : MFT.
    pub source: u8,
    pub journal_id: u64,
    pub next_usn: i64,
}

/// Index d'un volume.
#[derive(Clone, Debug)]
pub struct VolumeIndex {
    letter: char,
    pub meta: IndexMeta,
    items: Vec<Item>,
    names: Vec<u8>,
    by_rec: Vec<u32>,
    dead: usize,
}

fn flags_of(attributes: u32) -> u16 {
    let mut f = 0;
    if attributes & ATTR_DIRECTORY != 0 {
        f |= flag::DIR;
    }
    if attributes & ATTR_HIDDEN != 0 {
        f |= flag::HIDDEN;
    }
    if attributes & ATTR_SYSTEM != 0 {
        f |= flag::SYSTEM;
    }
    f
}

impl VolumeIndex {
    pub fn new(letter: char) -> Self {
        Self {
            letter: letter.to_ascii_uppercase(),
            meta: IndexMeta::default(),
            items: Vec::new(),
            names: Vec::new(),
            by_rec: Vec::new(),
            dead: 0,
        }
    }

    pub fn letter(&self) -> char {
        self.letter
    }

    /// Éléments présents (hors supprimés).
    pub fn len(&self) -> usize {
        self.items.len() - self.dead
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Mémoire occupée par l'index (octets).
    pub fn memory_bytes(&self) -> usize {
        self.items.capacity() * size_of::<Item>() + self.names.capacity() + self.by_rec.capacity() * 4
    }

    /// Réserve la place pour `items` éléments et `name_bytes` octets de noms.
    pub fn reserve(&mut self, items: usize, name_bytes: usize) {
        self.items.reserve(items);
        self.names.reserve(name_bytes);
    }

    fn slot(&self, rec: u32) -> Option<usize> {
        let i = *self.by_rec.get(rec as usize)?;
        (i != NONE).then_some(i as usize)
    }

    /// Ajoute ou met à jour l'élément `rec` (nom UTF-16 tel que sur le disque).
    pub fn upsert(&mut self, rec: u32, parent: u32, name: &[u16], attributes: u32) {
        if rec == NONE || name.is_empty() {
            return;
        }
        let (text, lossy) = match String::from_utf16(name) {
            Ok(s) => (s, false),
            Err(_) => (String::from_utf16_lossy(name), true),
        };
        let Ok(len) = u16::try_from(text.len()) else {
            return;
        };
        let mut flags = flags_of(attributes);
        if lossy {
            flags |= flag::LOSSY;
        }
        if let Some(i) = self.slot(rec) {
            let it = self.items[i];
            if it.parent == parent && self.name_bytes(i) == text.as_bytes() {
                self.items[i].flags = flags;
                return;
            }
            self.kill(i);
        }
        let Ok(off) = u32::try_from(self.names.len()) else {
            return;
        };
        if off.checked_add(len as u32).is_none() {
            return;
        }
        let Ok(index) = u32::try_from(self.items.len()) else {
            return;
        };
        if index == NONE {
            return;
        }
        grow(&mut self.names, text.len());
        self.names.extend_from_slice(text.as_bytes());
        grow(&mut self.items, 1);
        self.items.push(Item {
            rec,
            parent,
            off,
            len,
            flags,
        });
        let r = rec as usize;
        if r >= self.by_rec.len() {
            let extra = r + 1 - self.by_rec.len();
            grow(&mut self.by_rec, extra);
            self.by_rec.resize(r + 1, NONE);
        }
        self.by_rec[r] = index;
    }

    /// Élément indexé sous l'enregistrement `rec` : (parent, nom, dossier ?).
    pub fn item(&self, rec: u32) -> Option<(u32, &str, bool)> {
        let i = self.slot(rec)?;
        let it = &self.items[i];
        Some((it.parent, self.name(i), it.flags & flag::DIR != 0))
    }

    /// L'enregistrement `rec` est-il indexé ?
    pub fn contains(&self, rec: u32) -> bool {
        self.slot(rec).is_some()
    }

    /// Retire l'élément `rec` (supprimé du disque).
    pub fn remove(&mut self, rec: u32) {
        if let Some(i) = self.slot(rec) {
            self.kill(i);
        }
    }

    fn kill(&mut self, i: usize) {
        let it = &mut self.items[i];
        if it.flags & flag::DELETED != 0 {
            return;
        }
        it.flags |= flag::DELETED;
        let rec = it.rec as usize;
        self.dead += 1;
        if self.by_rec.get(rec) == Some(&(i as u32)) {
            self.by_rec[rec] = NONE;
        }
    }

    /// Enfant de `parent` nommé `name` (sans tenir compte de la casse), s'il est indexé.
    /// Parcourt tout l'index : réservé aux suppressions signalées par chemin.
    pub fn find_child(&self, parent: u32, name: &[u16]) -> Option<u32> {
        let wanted = String::from_utf16_lossy(name).to_lowercase();
        self.items.iter().enumerate().find_map(|(i, it)| {
            (it.parent == parent && it.flags & flag::DELETED == 0 && self.name(i).to_lowercase() == wanted)
                .then_some(it.rec)
        })
    }

    /// Retire les enfants désignés par (parent, nom en minuscules), en une seule passe.
    /// Renvoie les enregistrements des dossiers retirés (leurs descendants sont à retirer).
    pub fn remove_named(&mut self, wanted: &std::collections::HashSet<(u32, String)>) -> Vec<u32> {
        if wanted.is_empty() {
            return Vec::new();
        }
        let parents: std::collections::HashSet<u32> = wanted.iter().map(|(p, _)| *p).collect();
        let mut hit = Vec::new();
        for i in 0..self.items.len() {
            let it = self.items[i];
            if it.flags & flag::DELETED != 0 || !parents.contains(&it.parent) {
                continue;
            }
            if wanted.contains(&(it.parent, self.name(i).to_lowercase())) {
                hit.push(i);
            }
        }
        let mut dirs = Vec::new();
        for i in hit {
            if self.items[i].flags & flag::DIR != 0 {
                dirs.push(self.items[i].rec);
            }
            self.kill(i);
        }
        dirs
    }

    /// Retire tous les descendants des dossiers `dirs` (déjà retirés), en une seule passe.
    pub fn remove_under(&mut self, dirs: &std::collections::HashSet<u32>) {
        if dirs.is_empty() {
            return;
        }
        let mut doomed = Vec::new();
        for (i, it) in self.items.iter().enumerate() {
            if it.flags & flag::DELETED != 0 {
                continue;
            }
            let mut cur = it.parent;
            for _ in 0..MAX_DEPTH {
                if dirs.contains(&cur) {
                    doomed.push(i);
                    break;
                }
                if cur == ROOT_REC {
                    break;
                }
                match self.slot(cur) {
                    Some(p) => cur = self.items[p].parent,
                    None => break,
                }
            }
        }
        for i in doomed {
            self.kill(i);
        }
    }

    /// Numéros d'enregistrement des descendants de `rec` (pour retirer un dossier supprimé).
    pub fn descendants(&self, rec: u32) -> Vec<u32> {
        let mut out = Vec::new();
        for (i, it) in self.items.iter().enumerate() {
            if it.flags & flag::DELETED != 0 {
                continue;
            }
            let mut cur = it.parent;
            for _ in 0..MAX_DEPTH {
                if cur == rec {
                    out.push(self.items[i].rec);
                    break;
                }
                if cur == ROOT_REC {
                    break;
                }
                match self.slot(cur) {
                    Some(p) => cur = self.items[p].parent,
                    None => break,
                }
            }
        }
        out
    }

    fn name_bytes(&self, i: usize) -> &[u8] {
        let it = &self.items[i];
        &self.names[it.off as usize..it.off as usize + it.len as usize]
    }

    /// Nom de l'élément `i` (indice renvoyé par la recherche).
    pub fn name(&self, i: usize) -> &str {
        std::str::from_utf8(self.name_bytes(i)).unwrap_or("")
    }

    pub fn is_dir(&self, i: usize) -> bool {
        self.items.get(i).is_some_and(|it| it.flags & flag::DIR != 0)
    }

    pub fn flags(&self, i: usize) -> u16 {
        self.items.get(i).map_or(0, |it| it.flags)
    }

    /// Chemin complet de l'élément `i` (`C:\dossier\nom`). `None` si l'élément ou un ancêtre a
    /// été supprimé, ou si la chaîne des parents est rompue.
    pub fn path(&self, i: usize) -> Option<String> {
        let mut parts: Vec<&str> = Vec::new();
        let mut cur = i;
        for _ in 0..MAX_DEPTH {
            let it = self.items.get(cur)?;
            if it.flags & flag::DELETED != 0 {
                return None;
            }
            parts.push(self.name(cur));
            if it.parent == ROOT_REC {
                let mut s = format!("{}:", self.letter);
                for p in parts.iter().rev() {
                    s.push('\\');
                    s.push_str(p);
                }
                return Some(s);
            }
            if it.parent == it.rec {
                return None;
            }
            cur = self.slot(it.parent)?;
        }
        None
    }

    /// Récupère la place des éléments supprimés quand ils dépassent le quart de l'index.
    pub fn maybe_compact(&mut self) {
        if self.dead > 4096 && self.dead * 4 > self.items.len() {
            self.compact();
        }
    }

    pub fn compact(&mut self) {
        let mut items = Vec::with_capacity(self.len());
        let mut names = Vec::with_capacity(self.names.len());
        for (i, it) in self.items.iter().enumerate() {
            if it.flags & flag::DELETED != 0 {
                continue;
            }
            let off = names.len() as u32;
            names.extend_from_slice(self.name_bytes(i));
            items.push(Item { off, ..*it });
        }
        self.items = items;
        self.names = names;
        self.dead = 0;
        self.by_rec.iter_mut().for_each(|s| *s = NONE);
        for (i, it) in self.items.iter().enumerate() {
            if let Some(s) = self.by_rec.get_mut(it.rec as usize) {
                *s = i as u32;
            }
        }
    }

    /// Ajuste les capacités au contenu (après une construction complète).
    pub fn shrink(&mut self) {
        self.items.shrink_to_fit();
        self.names.shrink_to_fit();
        self.by_rec.shrink_to_fit();
    }
}

// ---------------------------------------------------------------------------- recherche

/// Requête : mots en minuscules, tous requis dans le nom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    words: Vec<String>,
    /// Mot cherché par balayage (le plus long).
    key: usize,
}

impl Query {
    pub fn parse(text: &str) -> Option<Self> {
        let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
        let key = words
            .iter()
            .enumerate()
            .max_by_key(|(_, w)| w.chars().count())
            .map(|(i, _)| i)?;
        Some(Self { words, key })
    }

    pub fn text(&self) -> String {
        self.words.join(" ")
    }
}

/// `s` commence par `w` (déjà en minuscules), sans tenir compte de la casse.
fn starts_folded(s: &str, w: &str) -> bool {
    let mut it = s.chars();
    for wc in w.chars() {
        let Some(c) = it.next() else { return false };
        if c == wc {
            continue;
        }
        if c.is_ascii() {
            if c.to_ascii_lowercase() != wc {
                return false;
            }
        } else {
            let mut l = c.to_lowercase();
            if l.next() != Some(wc) || l.next().is_some() {
                return false;
            }
        }
    }
    true
}

fn contains_folded(s: &str, w: &str) -> bool {
    s.char_indices().any(|(i, _)| starts_folded(&s[i..], w))
}

/// Premiers octets UTF-8 possibles du premier caractère de `w` (minuscule et majuscule).
fn lead_bytes(w: &str) -> Vec<u8> {
    let Some(c) = w.chars().next() else {
        return Vec::new();
    };
    let mut v = Vec::new();
    let mut add = |ch: char| {
        let mut buf = [0u8; 4];
        let b = ch.encode_utf8(&mut buf).as_bytes()[0];
        if !v.contains(&b) {
            v.push(b);
        }
    };
    add(c);
    let mut up = c.to_uppercase();
    if let (Some(u), None) = (up.next(), up.next()) {
        add(u);
    }
    v
}

/// Élément trouvé : volume (indice dans la liste passée à [`search`]) et élément.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub vol: u8,
    pub item: u32,
    rank: u8,
    len: u16,
}

#[derive(Clone, Debug, Default)]
pub struct Results {
    /// Nombre total d'éléments trouvés.
    pub total: usize,
    /// Les `limit` meilleurs, classés.
    pub hits: Vec<Hit>,
}

fn find_any(hay: &[u8], leads: &[u8]) -> Option<usize> {
    match *leads {
        [a] => memchr::memchr(a, hay),
        [a, b] => memchr::memchr2(a, b, hay),
        [a, b, c, ..] => memchr::memchr3(a, b, c, hay),
        [] => None,
    }
}

fn scan(ix: &VolumeIndex, vol: u8, range: std::ops::Range<usize>, q: &Query, leads: &[u8]) -> Vec<Hit> {
    let mut out = Vec::new();
    let Some(first) = ix.items.get(range.start) else {
        return out;
    };
    let Some(last) = ix.items.get(range.end - 1) else {
        return out;
    };
    let base = first.off as usize;
    let end = last.off as usize + last.len as usize;
    let hay = &ix.names[base..end];
    let key = &q.words[q.key];
    let single = q.words.len() == 1;
    let mut cur = range.start;
    let mut p = 0usize;
    while let Some(found) = find_any(&hay[p..], leads) {
        let abs = base + p + found;
        // Élément qui contient la position (les noms sont dans l'ordre des éléments).
        while cur < range.end && {
            let it = &ix.items[cur];
            it.off as usize + it.len as usize <= abs
        } {
            cur += 1;
        }
        if cur >= range.end {
            break;
        }
        let it = ix.items[cur];
        let item_end = it.off as usize + it.len as usize;
        let mut matched = false;
        if it.flags & flag::DELETED == 0
            && let Ok(tail) = std::str::from_utf8(&ix.names[abs..item_end])
            && starts_folded(tail, key)
        {
            let name = ix.name(cur);
            if q.words
                .iter()
                .enumerate()
                .all(|(i, w)| i == q.key || contains_folded(name, w))
            {
                matched = true;
                let prefix = abs == it.off as usize;
                let rank = if single && prefix && name.chars().count() == key.chars().count() {
                    0
                } else if prefix {
                    1
                } else {
                    2
                };
                out.push(Hit {
                    vol,
                    item: cur as u32,
                    rank,
                    len: it.len,
                });
            }
        }
        // Un élément trouvé n'est compté qu'une fois : on reprend après son nom.
        p = if matched { item_end - base } else { abs + 1 - base };
        if p >= hay.len() {
            break;
        }
    }
    out
}

/// Cherche `q` dans les index. `cancelled` est consulté entre les tranches ; `None` si la
/// recherche a été abandonnée (une frappe plus récente l'a remplacée).
pub fn search(
    indexes: &[&VolumeIndex],
    q: &Query,
    limit: usize,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<Results> {
    let leads = lead_bytes(&q.words[q.key]);
    let tasks: Vec<(u8, std::ops::Range<usize>)> = indexes
        .iter()
        .enumerate()
        .flat_map(|(v, ix)| {
            (0..ix.items.len())
                .step_by(CHUNK)
                .map(move |s| (v as u8, s..(s + CHUNK).min(ix.items.len())))
        })
        .collect();
    let parts: Vec<Option<Vec<Hit>>> = tasks
        .into_par_iter()
        .map(|(v, r)| (!cancelled()).then(|| scan(indexes[v as usize], v, r, q, &leads)))
        .collect();
    let mut hits = Vec::new();
    for p in parts {
        hits.extend(p?);
    }
    let total = hits.len();
    let key = |h: &Hit| (h.rank, h.len, h.vol, h.item);
    if hits.len() > limit {
        hits.select_nth_unstable_by_key(limit, key);
        hits.truncate(limit);
    }
    hits.sort_unstable_by_key(key);
    Some(Results { total, hits })
}

// ---------------------------------------------------------------------------- persistance

const MAGIC: &[u8; 8] = b"WDIDX001";

fn put_u32(w: &mut impl Write, v: u32) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn get<const N: usize>(r: &mut impl Read) -> io::Result<[u8; N]> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)?;
    Ok(b)
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("index invalide : {what}"))
}

impl VolumeIndex {
    /// Compacte l'index puis l'écrit : en-tête, éléments, noms.
    pub fn write_to(&mut self, w: &mut impl Write) -> io::Result<()> {
        self.compact();
        let ix = &*self;
        w.write_all(MAGIC)?;
        put_u32(w, ix.letter as u32)?;
        put_u32(w, ix.meta.serial)?;
        w.write_all(&[ix.meta.source])?;
        w.write_all(&ix.meta.journal_id.to_le_bytes())?;
        w.write_all(&ix.meta.next_usn.to_le_bytes())?;
        put_u32(w, ix.items.len() as u32)?;
        put_u32(w, ix.names.len() as u32)?;
        let mut buf = Vec::with_capacity(ix.items.len() * 16);
        for it in &ix.items {
            buf.extend_from_slice(&it.rec.to_le_bytes());
            buf.extend_from_slice(&it.parent.to_le_bytes());
            buf.extend_from_slice(&it.off.to_le_bytes());
            buf.extend_from_slice(&it.len.to_le_bytes());
            buf.extend_from_slice(&it.flags.to_le_bytes());
        }
        w.write_all(&buf)?;
        w.write_all(&ix.names)
    }

    /// Relit un index écrit par [`VolumeIndex::write_to`] en vérifiant chaque borne : un
    /// fichier abîmé est refusé (l'index sera reconstruit), jamais lu hors limites.
    pub fn read_from(r: &mut impl Read) -> io::Result<Self> {
        if &get::<8>(r)? != MAGIC {
            return Err(bad("en-tête"));
        }
        let letter = char::from_u32(u32::from_le_bytes(get(r)?))
            .filter(char::is_ascii_uppercase)
            .ok_or_else(|| bad("lettre"))?;
        let serial = u32::from_le_bytes(get(r)?);
        let [source] = get::<1>(r)?;
        let journal_id = u64::from_le_bytes(get(r)?);
        let next_usn = i64::from_le_bytes(get(r)?);
        let n = u32::from_le_bytes(get(r)?) as usize;
        let names_len = u32::from_le_bytes(get(r)?) as usize;
        if n > 200_000_000 || names_len > 4_000_000_000 {
            return Err(bad("taille"));
        }
        let mut raw = vec![0u8; n * 16];
        r.read_exact(&mut raw)?;
        let mut names = vec![0u8; names_len];
        r.read_exact(&mut names)?;
        let mut ix = VolumeIndex::new(letter);
        ix.meta = IndexMeta {
            serial,
            source,
            journal_id,
            next_usn,
        };
        ix.items.reserve_exact(n);
        let mut expected_off = 0usize;
        for c in raw.as_chunks::<16>().0 {
            let u32_at = |a: usize| u32::from_le_bytes([c[a], c[a + 1], c[a + 2], c[a + 3]]);
            let it = Item {
                rec: u32_at(0),
                parent: u32_at(4),
                off: u32_at(8),
                len: u16::from_le_bytes([c[12], c[13]]),
                flags: u16::from_le_bytes([c[14], c[15]]),
            };
            let end = it.off as usize + it.len as usize;
            if it.off as usize != expected_off
                || end > names.len()
                || it.rec == NONE
                || it.flags & flag::DELETED != 0
                || std::str::from_utf8(&names[it.off as usize..end]).is_err()
            {
                return Err(bad("élément"));
            }
            expected_off = end;
            ix.items.push(it);
        }
        if expected_off != names.len() {
            return Err(bad("noms"));
        }
        ix.names = names;
        let max_rec = ix
            .items
            .iter()
            .map(|it| it.rec as usize)
            .max()
            .map_or(0, |m| m + 1);
        ix.by_rec = vec![NONE; max_rec];
        for (i, it) in ix.items.iter().enumerate() {
            ix.by_rec[it.rec as usize] = i as u32;
        }
        Ok(ix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn sample() -> VolumeIndex {
        let mut ix = VolumeIndex::new('c');
        ix.upsert(10, ROOT_REC, &w("Users"), ATTR_DIRECTORY);
        ix.upsert(11, 10, &w("Moi"), ATTR_DIRECTORY);
        ix.upsert(12, 11, &w("Rapport Été.docx"), 0);
        ix.upsert(13, 11, &w("rapport.txt"), 0);
        ix.upsert(14, 11, &w("RAPPORT"), ATTR_DIRECTORY);
        ix.upsert(15, 14, &w("annexe-rapport.pdf"), ATTR_HIDDEN);
        ix
    }

    fn names(ix: &VolumeIndex, text: &str) -> Vec<String> {
        let q = Query::parse(text).expect("requête");
        let r = search(&[ix], &q, 100, &|| false).expect("non annulée");
        r.hits
            .iter()
            .map(|h| ix.name(h.item as usize).to_string())
            .collect()
    }

    #[test]
    fn finds_case_insensitively_and_ranks() {
        let ix = sample();
        assert_eq!(
            names(&ix, "rapport"),
            ["RAPPORT", "rapport.txt", "Rapport Été.docx", "annexe-rapport.pdf"]
        );
        assert_eq!(names(&ix, "été"), ["Rapport Été.docx"]);
        assert_eq!(names(&ix, "ÉTÉ rap"), ["Rapport Été.docx"]);
        assert_eq!(names(&ix, "pdf annexe"), ["annexe-rapport.pdf"]);
        assert!(names(&ix, "introuvable").is_empty());
    }

    #[test]
    fn builds_paths_and_follows_updates() {
        let mut ix = sample();
        let find = |ix: &VolumeIndex, t: &str| {
            let q = Query::parse(t).expect("requête");
            let r = search(&[ix], &q, 10, &|| false).expect("non annulée");
            r.hits.first().map(|h| ix.path(h.item as usize))
        };
        assert_eq!(
            find(&ix, "annexe"),
            Some(Some(r"C:\Users\Moi\RAPPORT\annexe-rapport.pdf".into()))
        );
        // Renommage du dossier parent : le chemin suit.
        ix.upsert(14, 11, &w("Archives"), ATTR_DIRECTORY);
        assert_eq!(
            find(&ix, "annexe"),
            Some(Some(r"C:\Users\Moi\Archives\annexe-rapport.pdf".into()))
        );
        assert_eq!(names(&ix, "archives"), ["Archives"]);
        assert!(!names(&ix, "rapport").contains(&"RAPPORT".to_string()));
        // Déplacement.
        ix.upsert(15, 10, &w("annexe-rapport.pdf"), 0);
        assert_eq!(
            find(&ix, "annexe"),
            Some(Some(r"C:\Users\annexe-rapport.pdf".into()))
        );
        // Suppression d'un dossier : ses descendants n'ont plus de chemin.
        let mut d = ix.descendants(11);
        d.sort_unstable();
        assert_eq!(d, [12, 13, 14]);
        ix.remove(11);
        assert_eq!(find(&ix, "txt"), Some(None));
        assert_eq!(ix.find_child(10, &w("ANNEXE-RAPPORT.PDF")), Some(15));
    }

    #[test]
    fn cancellation_and_limit() {
        let ix = sample();
        let q = Query::parse("r").expect("requête");
        assert!(search(&[&ix], &q, 10, &|| true).is_none());
        let r = search(&[&ix], &q, 2, &|| false).expect("non annulée");
        assert_eq!(r.total, 5); // tous sauf « Moi »
        assert_eq!(r.hits.len(), 2);
    }

    #[test]
    fn compaction_and_persistence_roundtrip() -> io::Result<()> {
        let mut ix = sample();
        let mut gone = std::collections::HashSet::new();
        gone.insert((11u32, "rapport".to_string()));
        assert_eq!(ix.remove_named(&gone), [14]);
        ix.remove_under(&[14u32].into_iter().collect());
        assert!(names(&ix, "annexe").is_empty());
        ix.upsert(13, 11, &w("renommé.txt"), 0);
        ix.remove(12);
        ix.meta.next_usn = 42;
        let mut buf = Vec::new();
        ix.write_to(&mut buf)?;
        let back = VolumeIndex::read_from(&mut buf.as_slice())?;
        assert_eq!(back.len(), ix.len());
        assert_eq!(back.meta.next_usn, 42);
        assert_eq!(names(&back, "renommé"), ["renommé.txt"]);
        assert!(names(&back, "docx").is_empty());
        // Fichier tronqué ou abîmé : refusé.
        assert!(VolumeIndex::read_from(&mut &buf[..buf.len() - 3]).is_err());
        let mut bad = buf.clone();
        let items_at = 8 + 4 + 4 + 1 + 8 + 8 + 4 + 4;
        bad[items_at + 8] = 0xFF; // décalage du premier nom
        assert!(VolumeIndex::read_from(&mut bad.as_slice()).is_err());
        ix.compact();
        assert_eq!(names(&ix, "renommé"), ["renommé.txt"]);
        Ok(())
    }

    #[test]
    fn invalid_utf16_is_kept_lossy() {
        let mut ix = VolumeIndex::new('D');
        ix.upsert(20, ROOT_REC, &[0x61, 0xD800, 0x62], 0);
        let q = Query::parse("a").expect("requête");
        let r = search(&[&ix], &q, 10, &|| false).expect("non annulée");
        assert_eq!(r.total, 1);
        assert_ne!(ix.flags(r.hits[0].item as usize) & flag::LOSSY, 0);
    }
}
