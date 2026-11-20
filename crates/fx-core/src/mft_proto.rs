//! Protocole de l'assistant d'indexation élevé → interface (sens unique).
//!
//! Trame : `genre (u8)`, `lettre (u8, A-Z)`, `longueur (u32)`, puis la charge utile. Les
//! enregistrements portent les champs utiles du journal USN. La lecture vérifie chaque
//! longueur : l'interface ne fait pas confiance aux octets reçus.

use std::io::{self, Read, Write};

/// Charge utile maximale d'une trame.
pub const MAX_PAYLOAD: usize = 4 << 20;
/// Longueur maximale d'un nom (unités UTF-16).
const MAX_NAME: usize = 1024;

const BEGIN: u8 = 1;
const RECORDS: u8 = 2;
const END_ENUM: u8 = 3;
const CHANGES: u8 = 4;
const ERROR: u8 = 5;

/// Enregistrement de la MFT ou du journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rec {
    pub frn: u64,
    pub parent: u64,
    pub attributes: u32,
    /// Raisons du journal (0 pendant l'énumération de la MFT).
    pub reason: u32,
    pub name: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// Début de l'énumération d'un volume.
    Begin {
        letter: char,
        serial: u32,
        journal_id: u64,
        next_usn: i64,
    },
    /// Lot d'entrées de la MFT.
    Records { letter: char, records: Vec<Rec> },
    /// Énumération terminée.
    EndEnum { letter: char },
    /// Changements lus dans le journal, jusqu'à `next_usn`.
    Changes {
        letter: char,
        next_usn: i64,
        records: Vec<Rec>,
    },
    /// Volume illisible (accès refusé, journal absent…).
    Error { letter: char, message: String },
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("trame invalide : {what}"))
}

fn put_records(out: &mut Vec<u8>, records: &[Rec]) {
    out.extend_from_slice(&(records.len() as u32).to_le_bytes());
    for r in records {
        out.extend_from_slice(&r.frn.to_le_bytes());
        out.extend_from_slice(&r.parent.to_le_bytes());
        out.extend_from_slice(&r.attributes.to_le_bytes());
        out.extend_from_slice(&r.reason.to_le_bytes());
        let n = r.name.len().min(MAX_NAME);
        out.extend_from_slice(&(n as u16).to_le_bytes());
        for u in &r.name[..n] {
            out.extend_from_slice(&u.to_le_bytes());
        }
    }
}

/// Taille encodée d'un enregistrement (pour découper les lots sous `MAX_PAYLOAD`).
pub fn encoded_len(r: &Rec) -> usize {
    26 + 2 * r.name.len().min(MAX_NAME)
}

/// Écrit une trame.
pub fn write_frame(w: &mut impl Write, f: &Frame) -> io::Result<()> {
    let mut payload = Vec::new();
    let (kind, letter) = match f {
        Frame::Begin {
            letter,
            serial,
            journal_id,
            next_usn,
        } => {
            payload.extend_from_slice(&serial.to_le_bytes());
            payload.extend_from_slice(&journal_id.to_le_bytes());
            payload.extend_from_slice(&next_usn.to_le_bytes());
            (BEGIN, *letter)
        }
        Frame::Records { letter, records } => {
            put_records(&mut payload, records);
            (RECORDS, *letter)
        }
        Frame::EndEnum { letter } => (END_ENUM, *letter),
        Frame::Changes {
            letter,
            next_usn,
            records,
        } => {
            payload.extend_from_slice(&next_usn.to_le_bytes());
            put_records(&mut payload, records);
            (CHANGES, *letter)
        }
        Frame::Error { letter, message } => {
            let m = message.as_bytes();
            payload.extend_from_slice(&m[..m.len().min(4096)]);
            (ERROR, *letter)
        }
    };
    if payload.len() > MAX_PAYLOAD || !letter.is_ascii_uppercase() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "trame trop grande"));
    }
    let mut head = [0u8; 6];
    head[0] = kind;
    head[1] = letter as u8;
    head[2..6].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    w.write_all(&head)?;
    w.write_all(&payload)
}

struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.0.len() < n {
            return Err(bad("tronquée"));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().map_err(|_| bad("u16"))?,
        ))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| bad("u32"))?,
        ))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| bad("u64"))?,
        ))
    }
    fn records(&mut self) -> io::Result<Vec<Rec>> {
        let n = self.u32()? as usize;
        // Chaque enregistrement fait au moins 26 octets : borne avant toute allocation.
        if n > self.0.len() / 26 {
            return Err(bad("nombre d'enregistrements"));
        }
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let frn = self.u64()?;
            let parent = self.u64()?;
            let attributes = self.u32()?;
            let reason = self.u32()?;
            let len = self.u16()? as usize;
            if len == 0 || len > MAX_NAME {
                return Err(bad("nom"));
            }
            let raw = self.take(len * 2)?;
            let name = raw
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            out.push(Rec {
                frn,
                parent,
                attributes,
                reason,
                name,
            });
        }
        Ok(out)
    }
}

/// Lit une trame ; `Ok(None)` à la fin propre du flux (entre deux trames).
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut head = [0u8; 6];
    match r.read(&mut head[..1])? {
        0 => return Ok(None),
        _ => r.read_exact(&mut head[1..])?,
    }
    let letter = head[1] as char;
    if !letter.is_ascii_uppercase() {
        return Err(bad("lettre"));
    }
    let len = u32::from_le_bytes([head[2], head[3], head[4], head[5]]) as usize;
    if len > MAX_PAYLOAD {
        return Err(bad("taille"));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    let mut c = Cursor(&payload);
    let frame = match head[0] {
        BEGIN => Frame::Begin {
            letter,
            serial: c.u32()?,
            journal_id: c.u64()?,
            next_usn: c.u64()? as i64,
        },
        RECORDS => Frame::Records {
            letter,
            records: c.records()?,
        },
        END_ENUM => Frame::EndEnum { letter },
        CHANGES => Frame::Changes {
            letter,
            next_usn: c.u64()? as i64,
            records: c.records()?,
        },
        ERROR => {
            let m = c.take(c.0.len())?;
            Frame::Error {
                letter,
                message: String::from_utf8_lossy(m).into_owned(),
            }
        }
        _ => return Err(bad("genre")),
    };
    if !c.0.is_empty() {
        return Err(bad("octets en trop"));
    }
    Ok(Some(frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(frn: u64, name: &str) -> Rec {
        Rec {
            frn,
            parent: 5,
            attributes: 0x20,
            reason: 0,
            name: name.encode_utf16().collect(),
        }
    }

    #[test]
    fn roundtrip_all_frames() -> io::Result<()> {
        let frames = vec![
            Frame::Begin {
                letter: 'C',
                serial: 7,
                journal_id: 99,
                next_usn: 1234,
            },
            Frame::Records {
                letter: 'C',
                records: vec![rec(40, "été.txt"), rec(41, "Dossier")],
            },
            Frame::EndEnum { letter: 'C' },
            Frame::Changes {
                letter: 'C',
                next_usn: 2000,
                records: vec![rec(42, "nouveau")],
            },
            Frame::Error {
                letter: 'D',
                message: "Accès refusé".into(),
            },
        ];
        let mut buf = Vec::new();
        for f in &frames {
            write_frame(&mut buf, f)?;
        }
        let mut r = buf.as_slice();
        let mut back = Vec::new();
        while let Some(f) = read_frame(&mut r)? {
            back.push(f);
        }
        assert_eq!(back, frames);
        Ok(())
    }

    #[test]
    fn rejects_malformed_input() -> io::Result<()> {
        let mut buf = Vec::new();
        write_frame(
            &mut buf,
            &Frame::Records {
                letter: 'C',
                records: vec![rec(40, "a")],
            },
        )?;
        // Lettre invalide, genre inconnu, taille démesurée, nombre d'enregistrements
        // mensonger, trame tronquée.
        let mut b = buf.clone();
        b[1] = b'#';
        assert!(read_frame(&mut b.as_slice()).is_err());
        let mut b = buf.clone();
        b[0] = 77;
        assert!(read_frame(&mut b.as_slice()).is_err());
        let mut b = buf.clone();
        b[2..6].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_frame(&mut b.as_slice()).is_err());
        let mut b = buf.clone();
        b[6..10].copy_from_slice(&1_000_000u32.to_le_bytes());
        assert!(read_frame(&mut b.as_slice()).is_err());
        assert!(read_frame(&mut &buf[..buf.len() - 1]).is_err());
        Ok(())
    }
}
