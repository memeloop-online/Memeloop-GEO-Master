//! Server-side AEAD envelope for browser storage state and proxy credentials.
//! Callers must never log the plaintext or expose it through a public DTO.

use rand_core::{OsRng, RngCore};
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};

pub struct SecretEnvelope {
    key: [u8; 32],
}

impl SecretEnvelope {
    /// A persistent deployment must provide a stable 256-bit key. An absent,
    /// malformed or empty key is an error rather than a plaintext fallback.
    pub fn from_hex_key(value: &str) -> Result<Self, &'static str> {
        let decoded = hex::decode(value).map_err(|_| "invalid channel secret key encoding")?;
        let key: [u8; 32] = decoded
            .try_into()
            .map_err(|_| "channel secret key must be 32 bytes")?;
        Ok(Self { key })
    }

    /// Only for non-durable development mode. Restart loses all connections.
    pub fn ephemeral() -> Self {
        let mut key = [0_u8; 32];
        OsRng.fill_bytes(&mut key);
        Self { key }
    }

    pub fn seal(&self, associated_data: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, &'static str> {
        let mut nonce_bytes = [0_u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let cipher = LessSafeKey::new(
            UnboundKey::new(&aead::AES_256_GCM, &self.key).map_err(|_| "cipher setup failed")?,
        );
        let mut ciphertext = plaintext.to_vec();
        cipher
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce_bytes),
                Aad::from(associated_data),
                &mut ciphertext,
            )
            .map_err(|_| "secret encryption failed")?;
        let mut envelope = Vec::with_capacity(1 + nonce_bytes.len() + ciphertext.len());
        envelope.push(1); // envelope format version
        envelope.extend_from_slice(&nonce_bytes);
        envelope.extend_from_slice(&ciphertext);
        Ok(envelope)
    }

    pub fn open(&self, associated_data: &[u8], envelope: &[u8]) -> Result<Vec<u8>, &'static str> {
        if envelope.len() < 1 + 12 + 16 || envelope[0] != 1 {
            return Err("secret envelope invalid");
        }
        let cipher = LessSafeKey::new(
            UnboundKey::new(&aead::AES_256_GCM, &self.key).map_err(|_| "cipher setup failed")?,
        );
        let mut nonce = [0_u8; 12];
        nonce.copy_from_slice(&envelope[1..13]);
        let mut ciphertext = envelope[13..].to_vec();
        cipher
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(associated_data),
                &mut ciphertext,
            )
            .map(|plaintext| plaintext.to_vec())
            .map_err(|_| "secret envelope authentication failed")
    }
}

#[cfg(test)]
mod tests {
    use super::SecretEnvelope;

    #[test]
    fn scoped_aead_round_trip_and_tamper_rejection() {
        let cipher = SecretEnvelope::ephemeral();
        let sealed = cipher.seal(b"scope:account:session", b"private").unwrap();
        assert_ne!(sealed, b"private");
        assert_eq!(
            cipher.open(b"scope:account:session", &sealed).unwrap(),
            b"private"
        );
        assert!(cipher.open(b"scope:other:session", &sealed).is_err());
        let mut tampered = sealed;
        *tampered.last_mut().unwrap() ^= 1;
        assert!(cipher.open(b"scope:account:session", &tampered).is_err());
    }

    #[test]
    fn requires_exact_key_length() {
        assert!(SecretEnvelope::from_hex_key("short").is_err());
        assert!(SecretEnvelope::from_hex_key(&"00".repeat(32)).is_ok());
    }
}
