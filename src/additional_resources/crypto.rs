use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RemoteCrypto {
    pub key: String,
    pub iv: String,
}

impl Default for RemoteCrypto {
    fn default() -> Self {
        serde_json::from_str(include_str!("crypto_defaults.json"))
            .expect("bundled remote configuration settings must be valid")
    }
}

impl RemoteCrypto {
    pub fn decode(&self, body: &[u8]) -> Result<Value, String> {
        let key = STANDARD
            .decode(&self.key)
            .map_err(|_| "Invalid configuration key (expected Base64)")?;
        let iv = STANDARD
            .decode(&self.iv)
            .map_err(|_| "Invalid configuration IV (expected Base64)")?;
        let text =
            std::str::from_utf8(body).map_err(|_| "Encrypted configuration is not UTF-8 Base64")?;
        let encoded: Vec<_> = text
            .trim_start_matches('\u{feff}')
            .bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        let mut cipher = STANDARD
            .decode(encoded)
            .map_err(|_| "Configuration is not valid Base64")?;
        let decryptor = cbc::Decryptor::<aes::Aes256>::new_from_slices(&key, &iv)
            .map_err(|_| "Configuration requires a 32-byte key and 16-byte IV")?;
        let plain = decryptor.decrypt_padded_mut::<Pkcs7>(&mut cipher).map_err(
            |_| "Cannot decrypt remote configuration; check crypto settings for this release",
        )?;
        serde_json::from_slice(plain)
            .map_err(|e| format!("Invalid decrypted configuration JSON: {e}"))
    }
}
