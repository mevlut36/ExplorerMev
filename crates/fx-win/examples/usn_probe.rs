//! Sonde : que permet le volume C: au compte courant (lecture seule) ?
fn main() {
    match fx_win::usn::Volume::open('C') {
        Err(e) => println!("ouverture du volume C : {e}"),
        Ok(v) => {
            println!("ouverture : OK");
            match v.journal() {
                Ok(j) => {
                    println!("journal : OK (next_usn {})", j.next_usn);
                    let t = std::time::Instant::now();
                    let mut n = 0u64;
                    let r = v.enum_mft(j.next_usn, |_| n += 1);
                    println!("énumération MFT : {r:?}, {n} entrées en {:?}", t.elapsed());
                }
                Err(e) => println!("journal : {e}"),
            }
        }
    }
}
