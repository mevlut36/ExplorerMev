//! Sélection multiple à la manière de l'Explorateur.
//!
//! La sélection est indexée par **entrée** (stable quand l'ordre ou le filtre change) ; les
//! opérations reçoivent la vue courante (`visible` : indices d'entrées dans l'ordre affiché).
//! L'**ancre** est le point de départ des sélections par plage (Maj) ; le **focus** est l'élément
//! courant du clavier.

use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct Selection {
    bits: Vec<u64>,
    count: usize,
    anchor: Option<u32>,
    focus: Option<u32>,
}

fn row_of(visible: &[u32], entry: Option<u32>) -> Option<usize> {
    let e = entry?;
    visible.iter().position(|&v| v == e)
}

impl Selection {
    /// Sélection vide pour un dossier de `len` entrées.
    pub fn new(len: usize) -> Self {
        Self {
            bits: vec![0; len.div_ceil(64)],
            count: 0,
            anchor: None,
            focus: None,
        }
    }

    pub fn len_capacity(&self) -> usize {
        self.bits.len() * 64
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn is_selected(&self, entry: u32) -> bool {
        let i = entry as usize;
        self.bits.get(i / 64).is_some_and(|w| w & (1 << (i % 64)) != 0)
    }

    pub fn focus(&self) -> Option<u32> {
        self.focus
    }

    pub fn anchor(&self) -> Option<u32> {
        self.anchor
    }

    /// Ligne du focus dans la vue (`None` s'il n'y est pas : filtré ou absent).
    pub fn focus_row(&self, visible: &[u32]) -> Option<usize> {
        row_of(visible, self.focus)
    }

    fn set(&mut self, entry: u32, on: bool) {
        let i = entry as usize;
        if i / 64 >= self.bits.len() {
            return;
        }
        let mask = 1u64 << (i % 64);
        let w = &mut self.bits[i / 64];
        let was = *w & mask != 0;
        if on && !was {
            *w |= mask;
            self.count += 1;
        } else if !on && was {
            *w &= !mask;
            self.count -= 1;
        }
    }

    pub fn clear(&mut self) {
        self.bits.iter_mut().for_each(|w| *w = 0);
        self.count = 0;
    }

    /// Entrées sélectionnées, dans l'ordre d'affichage (seulement celles visibles).
    pub fn selected_in(&self, visible: &[u32]) -> Vec<u32> {
        visible.iter().copied().filter(|&e| self.is_selected(e)).collect()
    }

    /// Toutes les entrées sélectionnées, par index croissant.
    pub fn entries(&self) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.count);
        for (wi, &w) in self.bits.iter().enumerate() {
            let mut w = w;
            while w != 0 {
                let b = w.trailing_zeros();
                out.push((wi * 64) as u32 + b);
                w &= w - 1;
            }
        }
        out
    }

    fn select_range(&mut self, visible: &[u32], a: usize, b: usize) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        for &e in visible
            .get(lo..=hi.min(visible.len().saturating_sub(1)))
            .unwrap_or(&[])
        {
            self.set(e, true);
        }
    }

    /// Clic sur la ligne `row` avec les modificateurs (Ctrl : bascule ; Maj : plage).
    pub fn click(&mut self, visible: &[u32], row: usize, ctrl: bool, shift: bool) {
        let Some(&entry) = visible.get(row) else { return };
        if shift {
            let anchor_row = row_of(visible, self.anchor).unwrap_or(row);
            if !ctrl {
                self.clear();
            }
            self.select_range(visible, anchor_row, row);
            if self.anchor.is_none() {
                self.anchor = Some(entry);
            }
        } else if ctrl {
            let on = !self.is_selected(entry);
            self.set(entry, on);
            self.anchor = Some(entry);
        } else {
            self.clear();
            self.set(entry, true);
            self.anchor = Some(entry);
        }
        self.focus = Some(entry);
    }

    /// Clic droit : conserve la sélection si la ligne en fait partie, sinon la remplace.
    pub fn right_click(&mut self, visible: &[u32], row: usize) {
        if let Some(&entry) = visible.get(row)
            && !self.is_selected(entry)
        {
            self.click(visible, row, false, false);
        }
    }

    /// Ctrl+A.
    pub fn select_all(&mut self, visible: &[u32]) {
        for &e in visible {
            self.set(e, true);
        }
    }

    /// Déplacement clavier du focus vers `row` (bornée à la vue).
    /// Maj : plage depuis l'ancre ; Ctrl : focus seul ; sinon sélection simple.
    pub fn move_to(&mut self, visible: &[u32], row: usize, ctrl: bool, shift: bool) {
        if visible.is_empty() {
            return;
        }
        let row = row.min(visible.len() - 1);
        let entry = visible[row];
        if shift {
            let anchor_row = row_of(visible, self.anchor).unwrap_or(row);
            if !ctrl {
                self.clear();
            }
            self.select_range(visible, anchor_row, row);
            if self.anchor.is_none() {
                self.anchor = Some(entry);
            }
            self.focus = Some(entry);
        } else if ctrl {
            self.focus = Some(entry);
        } else {
            self.clear();
            self.set(entry, true);
            self.anchor = Some(entry);
            self.focus = Some(entry);
        }
    }

    /// Ctrl+Espace : bascule l'élément du focus.
    pub fn toggle_focus(&mut self) {
        if let Some(e) = self.focus {
            let on = !self.is_selected(e);
            self.set(e, on);
            self.anchor = Some(e);
        }
    }

    /// Sélection rectangulaire couvrant les lignes `a..=b`. `base` : sélection d'avant le
    /// rectangle, conservée si Ctrl est enfoncé.
    pub fn rectangle(&mut self, visible: &[u32], a: usize, b: usize, base: Option<&Selection>) {
        match base {
            Some(base) => {
                self.bits.clone_from(&base.bits);
                self.count = base.count;
            }
            None => self.clear(),
        }
        if !visible.is_empty() {
            self.select_range(visible, a.min(visible.len() - 1), b.min(visible.len() - 1));
            let last = visible[b.min(visible.len() - 1)];
            self.focus = Some(last);
            self.anchor = Some(visible[a.min(visible.len() - 1)]);
        }
    }

    /// Rectangle de sélection en vue icônes : les lignes `rows` (non contiguës), ajoutées à
    /// `base` (Ctrl) ou seules.
    pub fn select_rows(&mut self, visible: &[u32], rows: &[usize], base: Option<&Selection>) {
        match base {
            Some(base) => {
                self.bits.clone_from(&base.bits);
                self.count = base.count;
            }
            None => self.clear(),
        }
        for &r in rows {
            if let Some(&e) = visible.get(r) {
                self.set(e, true);
            }
        }
        if let (Some(&a), Some(&f)) = (
            rows.first().and_then(|&r| visible.get(r)),
            rows.last().and_then(|&r| visible.get(r)),
        ) {
            self.anchor = Some(a);
            self.focus = Some(f);
        }
    }

    /// Reporte la sélection sur un nouveau dossier (après une mise à jour) : `map` donne le
    /// nouvel index d'une ancienne entrée, s'il existe encore.
    pub fn remap(&self, new_len: usize, map: impl Fn(u32) -> Option<u32>) -> Selection {
        let mut out = Selection::new(new_len);
        for e in self.entries() {
            if let Some(n) = map(e) {
                out.set(n, true);
            }
        }
        out.anchor = self.anchor.and_then(&map);
        out.focus = self.focus.and_then(&map);
        out
    }

    /// Sélectionne exactement `entries` (focus et ancre sur la première).
    pub fn select_only(&mut self, entries: &[u32]) {
        self.clear();
        for &e in entries {
            self.set(e, true);
        }
        self.focus = entries.first().copied();
        self.anchor = self.focus;
    }
}

/// Saisie au clavier pour sauter à un élément (« type-ahead »).
#[derive(Clone, Debug, Default)]
pub struct TypeAhead {
    buffer: String,
    last: Option<Instant>,
}

/// Délai après lequel une nouvelle frappe recommence la recherche.
pub const TYPE_AHEAD_RESET: Duration = Duration::from_millis(1000);

impl TypeAhead {
    /// Ajoute `c` et renvoie la ligne à atteindre. `current` : ligne du focus ;
    /// `name_of(row)` : nom affiché de la ligne.
    pub fn push(
        &mut self,
        c: char,
        now: Instant,
        len: usize,
        current: Option<usize>,
        name_of: impl Fn(usize) -> String,
    ) -> Option<usize> {
        if self.last.is_none_or(|t| now.duration_since(t) > TYPE_AHEAD_RESET) {
            self.buffer.clear();
        }
        self.last = Some(now);
        self.buffer.extend(c.to_lowercase());
        if len == 0 {
            return None;
        }
        // La même lettre répétée fait défiler les éléments qui commencent par elle.
        let mut chars = self.buffer.chars();
        let first = chars.next()?;
        let repeated = chars.all(|x| x == first);
        let (prefix, start) = if repeated {
            (first.to_string(), current.map_or(0, |r| r + 1))
        } else {
            (self.buffer.clone(), current.unwrap_or(0))
        };
        (0..len)
            .map(|i| (start + i) % len)
            .find(|&row| name_of(row).to_lowercase().starts_with(&prefix))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vis(n: u32) -> Vec<u32> {
        (0..n).rev().collect() // ordre d'affichage différent de l'ordre des entrées
    }

    #[test]
    fn plain_click_selects_one() {
        let v = vis(10);
        let mut s = Selection::new(10);
        s.click(&v, 2, false, false);
        s.click(&v, 5, false, false);
        assert_eq!(s.selected_in(&v), vec![v[5]]);
        assert_eq!(s.focus_row(&v), Some(5));
    }

    #[test]
    fn ctrl_toggles_and_shift_extends_from_anchor() {
        let v = vis(10);
        let mut s = Selection::new(10);
        s.click(&v, 1, false, false);
        s.click(&v, 4, false, true); // Maj : 1..=4
        assert_eq!(s.selected_in(&v), vec![v[1], v[2], v[3], v[4]]);
        s.click(&v, 2, true, false); // Ctrl : retire 2, ancre = 2
        assert_eq!(s.count(), 3);
        s.click(&v, 7, true, true); // Ctrl+Maj : ajoute 2..=7 sans effacer
        assert_eq!(s.selected_in(&v), (1..=7).map(|r| v[r]).collect::<Vec<_>>());
    }

    #[test]
    fn keyboard_moves_and_extends() {
        let v = vis(6);
        let mut s = Selection::new(6);
        s.move_to(&v, 0, false, false);
        s.move_to(&v, 3, false, true);
        assert_eq!(s.count(), 4);
        s.move_to(&v, 5, true, false); // Ctrl : focus seul
        assert_eq!(s.count(), 4);
        assert_eq!(s.focus_row(&v), Some(5));
        s.toggle_focus();
        assert_eq!(s.count(), 5);
        s.move_to(&v, 99, false, false); // borné
        assert_eq!(s.selected_in(&v), vec![v[5]]);
    }

    #[test]
    fn select_all_and_entries() {
        let v = vis(130);
        let mut s = Selection::new(130);
        s.select_all(&v);
        assert_eq!(s.count(), 130);
        assert_eq!(s.entries(), (0..130).collect::<Vec<_>>());
    }

    #[test]
    fn right_click_keeps_existing_selection() {
        let v = vis(5);
        let mut s = Selection::new(5);
        s.click(&v, 0, false, false);
        s.click(&v, 2, true, false);
        s.right_click(&v, 2);
        assert_eq!(s.count(), 2);
        s.right_click(&v, 4);
        assert_eq!(s.selected_in(&v), vec![v[4]]);
    }

    #[test]
    fn rectangle_with_and_without_base() {
        let v = vis(10);
        let mut s = Selection::new(10);
        s.click(&v, 0, false, false);
        let base = s.clone();
        s.rectangle(&v, 6, 3, None);
        assert_eq!(s.selected_in(&v), vec![v[3], v[4], v[5], v[6]]);
        s.rectangle(&v, 3, 4, Some(&base));
        assert_eq!(s.selected_in(&v), vec![v[0], v[3], v[4]]);
    }

    #[test]
    fn select_rows_with_and_without_base() {
        let v = vis(10);
        let mut s = Selection::new(10);
        s.click(&v, 9, false, false);
        let base = s.clone();
        s.select_rows(&v, &[0, 1, 4, 5], None);
        assert_eq!(s.selected_in(&v), vec![v[0], v[1], v[4], v[5]]);
        assert_eq!(s.focus(), Some(v[5]));
        s.select_rows(&v, &[2, 42], Some(&base));
        assert_eq!(s.selected_in(&v), vec![v[2], v[9]]);
        assert_eq!(s.count(), 2);
    }

    #[test]
    fn remap_follows_entries() {
        let v = vis(4);
        let mut s = Selection::new(4);
        s.click(&v, 0, false, false); // entrée 3
        let r = s.remap(10, |e| (e == 3).then_some(8));
        assert!(r.is_selected(8));
        assert_eq!(r.focus(), Some(8));
        assert_eq!(r.count(), 1);
    }

    #[test]
    fn type_ahead_prefix_and_cycling() {
        let names = ["Alpha", "beta", "Bravo", "bêta", "Charlie"];
        let name_of = |r: usize| names[r].to_string();
        let t0 = Instant::now();
        let mut ta = TypeAhead::default();
        assert_eq!(ta.push('b', t0, 5, None, name_of), Some(1));
        // Même lettre répétée : élément suivant commençant par « b ».
        assert_eq!(ta.push('b', t0, 5, Some(1), name_of), Some(2));
        // Après une pause, nouvelle recherche par préfixe.
        let later = t0 + Duration::from_secs(2);
        assert_eq!(ta.push('c', later, 5, Some(2), name_of), Some(4));
        let mut ta = TypeAhead::default();
        ta.push('b', t0, 5, None, name_of);
        assert_eq!(ta.push('r', t0, 5, Some(1), name_of), Some(2));
        assert_eq!(ta.push('z', t0, 5, Some(2), name_of), None);
    }
}
