use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::error::{Error, Result};

#[derive(Clone)]
pub struct Identity {
    pub device_id: String,
    pub name: String,
    secret: [u8; 32],
    pub pubkey: [u8; 32],
}

impl Identity {
    pub fn generate(name: impl Into<String>) -> Result<Self> {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|err| Error::msg(err.to_string()))?;
        Ok(Self::from_secret(name, secret))
    }

    pub fn from_secret(name: impl Into<String>, secret: [u8; 32]) -> Self {
        let key = SigningKey::from_bytes(&secret);
        let pubkey = key.verifying_key().to_bytes();
        Self {
            device_id: device_id_from_pubkey(&pubkey),
            name: name.into(),
            secret,
            pubkey,
        }
    }

    pub fn secret_hex(&self) -> String {
        hex::encode(self.secret)
    }

    pub fn pubkey_hex(&self) -> String {
        hex::encode(self.pubkey)
    }

    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        SigningKey::from_bytes(&self.secret).sign(message).to_bytes()
    }
}

pub fn device_id_from_pubkey(pubkey: &[u8; 32]) -> String {
    hex::encode(&blake3::hash(pubkey).as_bytes()[..16])
}

pub fn verify(pubkey: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(pubkey) else {
        return false;
    };
    key.verify(message, &Signature::from_bytes(signature))
        .is_ok()
}

pub fn pairing_code(left: &str, right: &str) -> String {
    let (first, second) = if left < right {
        (left, right)
    } else {
        (right, left)
    };
    let mut hasher = blake3::Hasher::new();
    hasher.update(first.as_bytes());
    hasher.update(b"|");
    hasher.update(second.as_bytes());
    let hash = hasher.finalize();
    let bytes = hash.as_bytes();
    let value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) % 1_000_000;
    format!("{value:06}")
}

pub fn parse_hex_32(text: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(text).map_err(|err| Error::msg(err.to_string()))?;
    let array: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Error::msg("ключ должен быть 32 байта"))?;
    Ok(array)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_roundtrip_and_stable_code() {
        let identity = Identity::generate("laptop").unwrap();
        let signature = identity.sign(b"hello");
        assert!(verify(&identity.pubkey, b"hello", &signature));
        assert!(!verify(&identity.pubkey, b"hellp", &signature));
        assert_eq!(
            pairing_code("bbb", "aaa"),
            pairing_code("aaa", "bbb")
        );
        assert_eq!(pairing_code("aaa", "bbb").len(), 6);
    }
}
