//! Secrets at rest: an OIDC client secret now, registry passwords and Git tokens later. Each is sealed with
//! XChaCha20-Poly1305 under a key made on first start in the data folder (`secret.key`, 0600), so a copy of the database
//! alone (a backup sent somewhere) gives none of them away ("Secrets" in DECISIONS.md).

use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD as B64;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;

const PREFIX: &str = "sealed1:";

#[derive(Clone)]
pub struct Sealer {
    cipher: XChaCha20Poly1305,
}

impl Sealer {
    pub fn from_key(key: &[u8; 32]) -> Self {
        Sealer { cipher: XChaCha20Poly1305::new(key.into()) }
    }

    /// The key in `path`, made first if it is not there.
    pub fn open_or_make(path: &Path) -> Result<Self> {
        if !path.exists() {
            let mut key = [0u8; 32];
            rand::rng().fill_bytes(&mut key);
            crate::tls::write_private(path, &key)?;
            tracing::info!("made the key that seals secrets, {}", path.display());
        }
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| anyhow::anyhow!("{} is not a 32-byte key", path.display()))?;
        Ok(Self::from_key(&key))
    }

    #[cfg(test)]
    pub fn for_test() -> Self {
        Self::from_key(&[7u8; 32])
    }

    pub fn seal(&self, plain: &str) -> Result<String> {
        let mut nonce = [0u8; 24];
        rand::rng().fill_bytes(&mut nonce);
        let sealed = self.cipher.encrypt(XNonce::from_slice(&nonce), plain.as_bytes()).map_err(|_| anyhow::anyhow!("sealing failed"))?;
        let mut out = nonce.to_vec();
        out.extend(sealed);
        Ok(format!("{PREFIX}{}", B64.encode(out)))
    }

    pub fn open(&self, sealed: &str) -> Result<String> {
        let Some(b) = sealed.strip_prefix(PREFIX) else { bail!("not a sealed secret") };
        let raw = B64.decode(b).context("a sealed secret that is not base64")?;
        if raw.len() < 24 {
            bail!("a sealed secret too short to hold anything");
        }
        let (nonce, body) = raw.split_at(24);
        let plain = self.cipher.decrypt(XNonce::from_slice(nonce), body).map_err(|_| anyhow::anyhow!("a sealed secret that this key did not seal, or that was changed"))?;
        Ok(String::from_utf8(plain)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_secret_opens_with_its_key_and_hides_what_it_holds() {
        let s = Sealer::for_test();
        let sealed = s.seal("hunter2-is-not-a-secret").unwrap();
        assert!(sealed.starts_with(PREFIX));
        assert!(!sealed.contains("hunter2"));
        assert_ne!(sealed, s.seal("hunter2-is-not-a-secret").unwrap(), "each seal has its own nonce");
        assert_eq!(s.open(&sealed).unwrap(), "hunter2-is-not-a-secret");
    }

    #[test]
    fn another_key_or_a_changed_byte_opens_nothing() {
        let sealed = Sealer::for_test().seal("x").unwrap();
        assert!(Sealer::from_key(&[8u8; 32]).open(&sealed).is_err());
        let mut b = sealed.into_bytes();
        let last = b.len() - 2;
        b[last] = if b[last] == b'A' { b'B' } else { b'A' };
        assert!(Sealer::for_test().open(&String::from_utf8(b).unwrap()).is_err());
        assert!(Sealer::for_test().open("plain text").is_err());
    }

    #[test]
    fn the_key_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secret.key");
        let sealed = Sealer::open_or_make(&p).unwrap().seal("kept").unwrap();
        assert_eq!(Sealer::open_or_make(&p).unwrap().open(&sealed).unwrap(), "kept");
    }
}
