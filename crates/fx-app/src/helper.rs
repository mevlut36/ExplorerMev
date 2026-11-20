//! Assistant d'indexation élevé : `windir --index-helper <tube> <pid parent> <lettres>`.
//!
//! Lancé par l'interface avec élévation (invite UAC), à la demande de l'utilisateur. Il fait le
//! strict minimum avec ses droits :
//! - ses arguments sont vérifiés avant toute action (nom de tube de la forme attendue, pid,
//!   lettres A-Z sans doublon) ;
//! - il se connecte au tube créé par l'interface (réservé à l'utilisateur), **sans rien y
//!   lire** : aucune commande ne peut lui être envoyée ;
//! - il lit la MFT puis le journal USN des volumes demandés, en lecture seule, et écrit les
//!   noms trouvés dans le tube ;
//! - il s'arrête quand l'interface se ferme ou que le tube se rompt.

use std::process::ExitCode;
use std::time::Duration;

use fx_core::mft_proto::{Frame, MAX_PAYLOAD, Rec, encoded_len, write_frame};
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
            let mut records = Vec::new();
            match o
                .volume
                .read_journal(o.journal_id, o.next_usn, |r| records.push(to_rec(r)))
            {
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
    let mut b = Batcher {
        records: Vec::new(),
        bytes: 0,
    };
    let mut io_err: Option<std::io::Error> = None;
    let r = volume.enum_mft(journal.next_usn, |rec| {
        if io_err.is_some() {
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
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
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
