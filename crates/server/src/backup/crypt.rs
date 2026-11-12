//! A backup archive's encryption: a key from the passphrase by Argon2id, then XChaCha20-Poly1305 in chunks of 1 MiB,
//! each bound to its place and to the header, the last one marked as last, so a chunk changed, moved, dropped, or the
//! end cut off is found ("Backups" 3 in DECISIONS.md).
//!
//! ```text
//! magic    "FREIGHTDOCK-BKP" 0x01               16 bytes
//! m_cost   u32 LE (KiB)  t_cost  u32 LE  p  u32 LE   Argon2id's cost, 12 bytes
//! salt     16 random bytes
//! prefix   19 random bytes                   each chunk's nonce: prefix || counter u32 BE || last (0 or 1)
//! chunks   ciphertext of 1 MiB of plain (+16 tag) each; the last holds less than 1 MiB (perhaps nothing)
//! ```
//! The 63 header bytes are every chunk's associated data.

use anyhow::{Result, bail};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;

pub const MAGIC: &[u8; 16] = b"FREIGHTDOCK-BKP\x01";
const HEADER: usize = 16 + 12 + 16 + 19;
pub const CHUNK: usize = 1 << 20;
const TAG: usize = 16;

/// Argon2id's cost: memory in KiB, passes, lanes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cost {
    pub m: u32,
    pub t: u32,
    pub p: u32,
}

/// What a backup is sealed with: 64 MiB, three passes, one lane.
pub const COST: Cost = Cost { m: 64 * 1024, t: 3, p: 1 };

impl Cost {
    /// A header may not ask for more than this: a made-up archive must not make Freightdock spend a gigabyte or an hour.
    fn sane(&self) -> bool {
        (8..=1024 * 1024).contains(&self.m) && (1..=10).contains(&self.t) && (1..=8).contains(&self.p)
    }
}

fn key(passphrase: &str, salt: &[u8], cost: Cost) -> Result<XChaCha20Poly1305> {
    let params = argon2::Params::new(cost.m, cost.t, cost.p, Some(32)).map_err(|e| anyhow::anyhow!("Argon2's cost: {e}"))?;
    let mut k = [0u8; 32];
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params).hash_password_into(passphrase.as_bytes(), salt, &mut k).map_err(|e| anyhow::anyhow!("deriving the key: {e}"))?;
    Ok(XChaCha20Poly1305::new((&k).into()))
}

fn nonce(prefix: &[u8], i: u32, last: bool) -> XNonce {
    let mut n = [0u8; 24];
    n[..19].copy_from_slice(prefix);
    n[19..23].copy_from_slice(&i.to_be_bytes());
    n[23] = last as u8;
    *XNonce::from_slice(&n)
}

/// Seals `plain` under `passphrase`.
pub fn seal(plain: &[u8], passphrase: &str, cost: Cost) -> Result<Vec<u8>> {
    if passphrase.is_empty() {
        bail!("a backup needs a passphrase");
    }
    let mut salt = [0u8; 16];
    let mut prefix = [0u8; 19];
    rand::rng().fill_bytes(&mut salt);
    rand::rng().fill_bytes(&mut prefix);
    let mut header = Vec::with_capacity(HEADER);
    header.extend_from_slice(MAGIC);
    for v in [cost.m, cost.t, cost.p] {
        header.extend_from_slice(&v.to_le_bytes());
    }
    header.extend_from_slice(&salt);
    header.extend_from_slice(&prefix);
    let cipher = key(passphrase, &salt, cost)?;
    let mut out = Vec::with_capacity(HEADER + plain.len() + (plain.len() / CHUNK + 1) * TAG);
    out.extend_from_slice(&header);
    // Full chunks, then a last one shorter than a chunk: when the plain is a whole number of chunks, an empty last.
    let full = plain.len() / CHUNK;
    for i in 0..=full {
        let last = i == full;
        let piece = &plain[i * CHUNK..if last { plain.len() } else { (i + 1) * CHUNK }];
        let sealed = cipher.encrypt(&nonce(&prefix, i as u32, last), Payload { msg: piece, aad: &header }).map_err(|_| anyhow::anyhow!("sealing failed"))?;
        out.extend_from_slice(&sealed);
    }
    Ok(out)
}

/// Whether `data` starts as a Freightdock backup does.
pub fn is_backup(data: &[u8]) -> bool {
    data.len() >= HEADER && &data[..16] == MAGIC
}

/// Opens what `seal` made. A wrong passphrase and a changed archive are the same error: nothing tells them apart.
pub fn open(data: &[u8], passphrase: &str) -> Result<Vec<u8>> {
    if !is_backup(data) {
        bail!("this is not a Freightdock backup");
    }
    let header = &data[..HEADER];
    let word = |i: usize| u32::from_le_bytes(header[16 + i * 4..20 + i * 4].try_into().expect("four bytes"));
    let cost = Cost { m: word(0), t: word(1), p: word(2) };
    if !cost.sane() {
        bail!("this backup asks for a key cost Freightdock does not pay");
    }
    let (salt, prefix) = (&header[28..44], &header[44..63]);
    let cipher = key(passphrase, salt, cost)?;
    let body = &data[HEADER..];
    let full = body.len() / (CHUNK + TAG);
    let rest = body.len() - full * (CHUNK + TAG);
    // The last chunk is always shorter than a full one, so a body of only full chunks has lost its end.
    if rest < TAG {
        bail!("the passphrase is wrong, or the backup is damaged or cut short");
    }
    let mut out = Vec::with_capacity(body.len());
    for i in 0..=full {
        let last = i == full;
        let piece = &body[i * (CHUNK + TAG)..if last { body.len() } else { (i + 1) * (CHUNK + TAG) }];
        let plain = cipher.decrypt(&nonce(prefix, i as u32, last), Payload { msg: piece, aad: header }).map_err(|_| anyhow::anyhow!("the passphrase is wrong, or the backup is damaged or cut short"))?;
        out.extend_from_slice(&plain);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cheap enough for tests; the format is the same.
    const CHEAP: Cost = Cost { m: 64, t: 1, p: 1 };

    #[test]
    fn what_is_sealed_opens_with_its_passphrase_only() {
        let plain = b"the database, the key, the CA".to_vec();
        let sealed = seal(&plain, "correct horse", CHEAP).unwrap();
        assert!(is_backup(&sealed));
        assert!(!sealed.windows(8).any(|w| w == b"database"), "nothing of it shows");
        assert_eq!(open(&sealed, "correct horse").unwrap(), plain);
        assert!(open(&sealed, "wrong horse").is_err());
        assert_ne!(seal(&plain, "correct horse", CHEAP).unwrap(), sealed, "a salt and nonces of its own each time");
        assert!(seal(&plain, "", CHEAP).is_err());
    }

    #[test]
    fn many_chunks_round_trip_and_a_whole_number_of_them_too() {
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK] {
            let plain: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let sealed = seal(&plain, "p", CHEAP).unwrap();
            assert_eq!(open(&sealed, "p").unwrap(), plain, "{len} bytes");
        }
    }

    #[test]
    fn a_changed_moved_or_cut_archive_does_not_open() {
        let plain: Vec<u8> = (0..(2 * CHUNK + 10)).map(|i| (i % 7) as u8).collect();
        let sealed = seal(&plain, "p", CHEAP).unwrap();
        let mut changed = sealed.clone();
        changed[HEADER + 5] ^= 1;
        assert!(open(&changed, "p").is_err());
        let mut header = sealed.clone();
        header[30] ^= 1;
        assert!(open(&header, "p").is_err(), "the header is bound to every chunk");
        // Cut at a chunk's end: what is left looks whole, but its last chunk was not sealed as last.
        assert!(open(&sealed[..HEADER + 2 * (CHUNK + TAG)], "p").is_err());
        assert!(open(&sealed[..sealed.len() - 1], "p").is_err());
        // Two chunks swapped.
        let mut swapped = sealed[..HEADER].to_vec();
        swapped.extend_from_slice(&sealed[HEADER + CHUNK + TAG..HEADER + 2 * (CHUNK + TAG)]);
        swapped.extend_from_slice(&sealed[HEADER..HEADER + CHUNK + TAG]);
        swapped.extend_from_slice(&sealed[HEADER + 2 * (CHUNK + TAG)..]);
        assert!(open(&swapped, "p").is_err());
        assert!(open(b"not a backup at all, just words", "p").is_err());
    }

    #[test]
    fn a_header_asking_for_too_much_is_refused_before_any_work() {
        let mut sealed = seal(b"x", "p", CHEAP).unwrap();
        sealed[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        let e = open(&sealed, "p").unwrap_err().to_string();
        assert!(e.contains("cost"), "{e}");
    }
}
