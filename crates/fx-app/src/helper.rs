//! Assistant d'indexation élevé : `explorermev --index-helper <tube> <pid parent> <lettres>`.
//!
//! Lancé par l'interface avec élévation (invite UAC), à la demande de l'utilisateur. Il fait le
//! strict minimum avec ses droits :
//! - ses arguments sont vérifiés avant toute action (nom de tube de la forme attendue, pid,
//!   lettres A-Z sans doublon) ;
//! - il se connecte au tube créé par l'interface (réservé à l'utilisateur), **sans rien y
//!   lire** : aucune commande ne peut lui être envoyée ;
//! - il lit la MFT puis le journal USN des volumes demandés, en lecture seule, et écrit dans
//!   le tube les noms **dont le dossier parent est listable par le compte non élevé de
//!   l'utilisateur** (`AccessCheck` avec le jeton lié, une fois par descripteur de sécurité) :
//!   l'index ne contient rien de plus que ce qu'un parcours des dossiers sans droits verrait ;
//! - il s'arrête quand l'interface se ferme ou que le tube se rompt.

use std::collections::HashMap;
use std::process::ExitCode;
use std::time::Duration;

use fx_core::mft_proto::{Frame, MAX_PAYLOAD, Rec, encoded_len, write_frame};
use fx_win::access::{ListChecker, SecurityDescriptor};
use fx_win::pipe::{PipeClient, is_valid_pipe_name, process_alive};
use fx_win::usn::{UsnRecord, Volume};

/// Arguments validés.
#[derive(Debug, PartialEq, Eq)]
pub struct HelperArgs {
    pub pipe: String,
    pub parent: u32,
    pub letters: Vec<char>,
}

pub fn parse(args: &[String]) -> Option<HelperArgs> {
    let [pipe, parent, letters] = args else {
        return None;
    };
    if !is_valid_pipe_name(pipe) {
        return None;
    }
    let parent: u32 = parent.parse().ok().filter(|&p| p != 0)?;
    let mut out: Vec<char> = Vec::new();
    for c in letters.chars() {
        if !c.is_ascii_uppercase() || out.contains(&c) {
            return None;
        }
        out.push(c);
    }
    (!out.is_empty()).then_some(HelperArgs {
        pipe: pipe.clone(),
        parent,
        letters: out,
    })
}

fn to_rec(r: UsnRecord) -> Rec {
    Rec {
        frn: r.frn,
        parent: r.parent,
        attributes: r.attributes,
        reason: r.reason,
        name: r.name,
    }
}

/// Lots sous la taille maximale d'une trame.
struct Batcher {
    records: Vec<Rec>,
    bytes: usize,
}

impl Batcher {
    fn push(&mut self, r: Rec) -> bool {
        self.bytes += encoded_len(&r);
        self.records.push(r);
        self.bytes > MAX_PAYLOAD / 2
    }
    fn take(&mut self) -> Vec<Rec> {
        self.bytes = 0;
        std::mem::take(&mut self.records)
    }
}

struct Open {
    letter: char,
    volume: Volume,
    journal_id: u64,
    next_usn: i64,
    filter: Filter,
    checker: ListChecker,
}

/// Contrôle réel : descripteur du dossier lu par identifiant, `AccessCheck` avec le jeton non
/// élevé. Toute erreur vaut refus.
fn probe<'a>(vol: &'a Volume, checker: &'a ListChecker) -> impl FnMut(u64) -> bool + 'a {
    move |frn| {
        vol.security_of(frn)
            .map(|sd| checker.can_list(&sd))
            .unwrap_or(false)
    }
}

const ATTR_DIRECTORY: u32 = 0x10;
const ROOT_REC: u64 = 5;
const FILE_DELETE: u32 = 0x0000_0200;

fn rec_of(frn: u64) -> u64 {
    frn & 0xFFFF_FFFF_FFFF
}

/// Dossiers du volume et droit de les lister (jeton non élevé de l'utilisateur).
///
/// Un nom n'est transmis que si **tous** les dossiers de son chemin sont listables : c'est ce
/// qu'un parcours des dossiers sans droits verrait. Dans le doute (descripteur illisible,
/// dossier inconnu, boucle de parents), la réponse est non.
struct Filter {
    root_ok: bool,
    /// Dossier (enregistrement) → (référence complète, référence du parent, identifiant de
    /// sécurité).
    dirs: HashMap<u64, (u64, u64, u32)>,
    /// Identifiant de sécurité (non nul) → listable.
    by_sid: HashMap<u32, bool>,
    /// Dossier (enregistrement) → chemin entièrement listable. Vidé quand un dossier est
    /// déplacé ou change de droits.
    by_dir: HashMap<u64, bool>,
}

const RENAME_NEW_NAME: u32 = 0x0000_2000;
const SECURITY_CHANGE: u32 = 0x0000_0800;
const MAX_DEPTH: usize = 512;

impl Filter {
    fn new(root_ok: bool) -> Self {
        Self {
            root_ok,
            dirs: HashMap::new(),
            by_sid: HashMap::new(),
            by_dir: HashMap::new(),
        }
    }

    fn note_dir(&mut self, r: &UsnRecord) {
        if r.attributes & ATTR_DIRECTORY == 0 {
            return;
        }
        let rec = rec_of(r.frn);
        let known = self.dirs.get(&rec).map(|&(_, parent, _)| parent);
        self.dirs.insert(rec, (r.frn, r.parent, r.security_id));
        // Déplacement ou nouveaux droits : les réponses en cache peuvent être fausses.
        if r.reason & (RENAME_NEW_NAME | SECURITY_CHANGE) != 0 || known.is_some_and(|p| p != r.parent) {
            self.by_dir.clear();
        }
    }

    /// Le dossier `frn` (lui seul) est-il listable ? `probe` fait le contrôle réel.
    fn listable(&mut self, frn: u64, sid: u32, probe: &mut impl FnMut(u64) -> bool) -> bool {
        if sid != 0
            && let Some(&ok) = self.by_sid.get(&sid)
        {
            return ok;
        }
        let ok = probe(frn);
        if sid != 0 {
            self.by_sid.insert(sid, ok);
        }
        ok
    }

    /// Les entrées du dossier `parent` (référence) peuvent-elles être transmises ?
    fn allowed(&mut self, parent: u64, probe: &mut impl FnMut(u64) -> bool) -> bool {
        // Chaîne des dossiers jusqu'à la racine ou jusqu'à un dossier déjà évalué.
        let mut chain: Vec<u64> = Vec::new();
        let mut cur = parent;
        let mut verdict: Option<bool> = None;
        for _ in 0..MAX_DEPTH {
            let rec = rec_of(cur);
            if rec == ROOT_REC {
                verdict = Some(self.root_ok);
                break;
            }
            if let Some(&ok) = self.by_dir.get(&rec) {
                verdict = Some(ok);
                break;
            }
            chain.push(cur);
            match self.dirs.get(&rec) {
                Some(&(_, up, _)) => cur = up,
                // Dossier jamais vu : on ne connaît pas ses ancêtres.
                None => {
                    verdict = Some(false);
                    break;
                }
            }
        }
        // Boucle ou chaîne trop longue : refus.
        let mut ok = verdict.unwrap_or(false);
        // Du haut vers le bas : un dossier n'est accepté que si ses ancêtres le sont.
        for &frn in chain.iter().rev() {
            let rec = rec_of(frn);
            if ok {
                let (full, sid) = self.dirs.get(&rec).map_or((frn, 0), |&(f, _, s)| (f, s));
                ok = self.listable(full, sid, probe);
            }
            self.by_dir.insert(rec, ok);
        }
        ok
    }
}

pub fn run(args: &[String]) -> ExitCode {
    let Some(a) = parse(args) else {
        return ExitCode::from(2);
    };
    let Ok(mut out) = PipeClient::connect(&a.pipe) else {
        return ExitCode::from(3);
    };
    let mut open: Vec<Open> = Vec::new();
    for &letter in &a.letters {
        match enumerate(letter, &mut out) {
            Ok(Some(o)) => open.push(o),
            Ok(None) => {}
            Err(_) => return ExitCode::from(4), // tube rompu
        }
    }
    if open.is_empty() {
        return ExitCode::SUCCESS;
    }
    // Suivi du journal tant que l'interface vit.
    loop {
        std::thread::sleep(Duration::from_millis(1000));
        if !process_alive(a.parent) {
            return ExitCode::SUCCESS;
        }
        for o in &mut open {
            let mut raw = Vec::new();
            let result = o.volume.read_journal(o.journal_id, o.next_usn, |r| raw.push(r));
            // Même filtre que l'énumération ; une suppression passe toujours (elle ne révèle
            // aucun nom).
            let mut records = Vec::new();
            for r in raw {
                if r.reason & FILE_DELETE == 0 {
                    o.filter.note_dir(&r);
                    if !o.filter.allowed(r.parent, &mut probe(&o.volume, &o.checker)) {
                        continue;
                    }
                }
                records.push(to_rec(r));
            }
            match result {
                Ok(next) => {
                    if next == o.next_usn && records.is_empty() {
                        continue;
                    }
                    o.next_usn = next;
                    // Découpage sous la taille maximale d'une trame.
                    let mut b = Batcher {
                        records: Vec::new(),
                        bytes: 0,
                    };
                    let mut chunks = Vec::new();
                    for r in records {
                        if b.push(r) {
                            chunks.push(b.take());
                        }
                    }
                    chunks.push(b.take());
                    for records in chunks {
                        let f = Frame::Changes {
                            letter: o.letter,
                            next_usn: next,
                            records,
                        };
                        if write_frame(&mut out, &f).is_err() {
                            return ExitCode::SUCCESS;
                        }
                    }
                }
                Err(e) => {
                    let f = Frame::Error {
                        letter: o.letter,
                        message: format!("journal : {e}"),
                    };
                    if write_frame(&mut out, &f).is_err() {
                        return ExitCode::SUCCESS;
                    }
                }
            }
        }
    }
}

/// Énumère la MFT d'un volume. `Ok(None)` : volume illisible (erreur transmise).
fn enumerate(letter: char, out: &mut PipeClient) -> std::io::Result<Option<Open>> {
    let fail = |out: &mut PipeClient, e: std::io::Error| {
        write_frame(
            out,
            &Frame::Error {
                letter,
                message: e.to_string(),
            },
        )
        .map(|()| None)
    };
    let volume = match Volume::open(letter) {
        Ok(v) => v,
        Err(e) => return fail(out, e),
    };
    let journal = match volume.journal() {
        Ok(j) => j,
        Err(e) => return fail(out, e),
    };
    let serial =
        fx_win::listing::file_ref(std::path::Path::new(&format!("{letter}:\\"))).map_or(0, |(_, s)| s);
    write_frame(
        out,
        &Frame::Begin {
            letter,
            serial,
            journal_id: journal.id,
            next_usn: journal.next_usn,
        },
    )?;
    // Premier passage : les dossiers et leur descripteur de sécurité.
    let checker = match ListChecker::limited_user() {
        Ok(c) => c,
        Err(e) => return fail(out, e),
    };
    let root_ok = SecurityDescriptor::of_path(std::path::Path::new(&format!("{letter}:\\")))
        .map(|sd| checker.can_list(&sd))
        .unwrap_or(false);
    let mut filter = Filter::new(root_ok);
    if let Err(e) = volume.enum_mft(journal.next_usn, |rec| filter.note_dir(&rec)) {
        return fail(out, e);
    }
    // Second passage : seulement les entrées des dossiers listables.
    let mut b = Batcher {
        records: Vec::new(),
        bytes: 0,
    };
    let mut io_err: Option<std::io::Error> = None;
    let r = volume.enum_mft(journal.next_usn, |rec| {
        if io_err.is_some() || !filter.allowed(rec.parent, &mut probe(&volume, &checker)) {
            return;
        }
        if b.push(to_rec(rec))
            && let Err(e) = write_frame(
                out,
                &Frame::Records {
                    letter,
                    records: b.take(),
                },
            )
        {
            io_err = Some(e);
        }
    });
    if let Some(e) = io_err {
        return Err(e);
    }
    if let Err(e) = r {
        return fail(out, e);
    }
    write_frame(
        out,
        &Frame::Records {
            letter,
            records: b.take(),
        },
    )?;
    write_frame(out, &Frame::EndEnum { letter })?;
    Ok(Some(Open {
        letter,
        volume,
        journal_id: journal.id,
        next_usn: journal.next_usn,
        filter,
        checker,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn dir(frn: u64, parent: u64, reason: u32) -> UsnRecord {
        UsnRecord {
            frn,
            parent,
            usn: 0,
            reason,
            attributes: ATTR_DIRECTORY,
            security_id: 0,
            name: vec![0x61],
        }
    }

    #[test]
    fn filter_requires_every_ancestor_to_be_listable() {
        // 5 (racine) ─ 100 Users ─ 101 Autre (refusé) ─ 102 Public
        //                        └ 103 Moi ─ 104 Documents
        let mut f = Filter::new(true);
        for r in [
            dir(100, 5, 0),
            dir(101, 100, 0),
            dir(102, 101, 0),
            dir(103, 100, 0),
            dir(104, 103, 0),
        ] {
            f.note_dir(&r);
        }
        let calls = std::cell::Cell::new(0);
        let mut probe = |frn: u64| {
            calls.set(calls.get() + 1);
            frn != 101
        };
        assert!(f.allowed(5, &mut probe));
        assert!(f.allowed(104, &mut probe));
        assert!(!f.allowed(101, &mut probe));
        // Public est listable, mais sous un dossier qui ne l'est pas : refusé.
        assert!(!f.allowed(102, &mut probe));
        // Dossier inconnu : refusé.
        assert!(!f.allowed(999, &mut probe));
        // Réponses en cache : pas de nouveau contrôle.
        let before = calls.get();
        assert!(f.allowed(104, &mut probe));
        assert_eq!(calls.get(), before);
        // Documents déplacé sous Autre : le cache est vidé, la réponse change.
        f.note_dir(&dir(104, 101, 0x2000));
        assert!(!f.allowed(104, &mut probe));
    }

    #[test]
    fn filter_refuses_loops_and_unreadable_root() {
        let mut f = Filter::new(true);
        f.note_dir(&dir(200, 201, 0));
        f.note_dir(&dir(201, 200, 0));
        assert!(!f.allowed(200, &mut |_| true));
        let mut closed = Filter::new(false);
        closed.note_dir(&dir(300, 5, 0));
        assert!(!closed.allowed(300, &mut |_| true));
        assert!(!closed.allowed(5, &mut |_| true));
    }

    #[test]
    fn arguments_are_strictly_validated() {
        let pipe = fx_win::pipe::random_pipe_name();
        let ok = parse(&s(&[&pipe, "1234", "CD"]));
        assert_eq!(
            ok,
            Some(HelperArgs {
                pipe: pipe.clone(),
                parent: 1234,
                letters: vec!['C', 'D']
            })
        );
        for bad in [
            s(&[&pipe, "1234"]),
            s(&[&pipe, "1234", "CD", "x"]),
            s(&[r"\\.\pipe\autre", "1234", "C"]),
            s(&[&pipe, "0", "C"]),
            s(&[&pipe, "-1", "C"]),
            s(&[&pipe, "1234", "c"]),
            s(&[&pipe, "1234", "CC"]),
            s(&[&pipe, "1234", ""]),
            s(&[&pipe, "1234", "C:"]),
        ] {
            assert_eq!(parse(&bad), None, "{bad:?}");
        }
    }
}
