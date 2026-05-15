use chacha20::{
    ChaCha20,
    cipher::{KeyIvInit, StreamCipher, StreamCipherSeek},
};
use rand::Rng;
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct Crypto {
    /// ChaCha20 standardizes on 256-bit key
    key: [u8; 32],
    /// ChaCha20 standardizes on 96-bit nonce
    pub nonce: [u8; 12],
}

impl Crypto {
    pub fn new(password: &str, nonce: [u8; 12]) -> Self {
        let hash = Sha256::digest(password.as_bytes());
        let mut key = [0u8; 32];
        key.copy_from_slice(&hash);
        Self { key, nonce }
    }

    pub fn apply(&self, addr: u64, data: &mut [u8]) {
        let mut cipher = ChaCha20::new(&self.key.into(), &self.nonce.into());
        cipher.seek(addr);
        cipher.apply_keystream(data);
    }

    pub fn gen_nonce() -> [u8; 12] {
        let mut nonce = [0u8; 12];
        let mut rng = rand::rng();
        rng.fill_bytes(&mut nonce);
        nonce
    }
}
