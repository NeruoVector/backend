use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, AeadCore, OsRng, Payload},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{cards::CardMetadata, error::AppError};

#[derive(Clone)]
pub struct CardMetadataKey {
    cipher: Aes256Gcm,
    id: String,
}
impl std::fmt::Debug for CardMetadataKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CardMetadataKey([REDACTED])")
    }
}
fn unavailable() -> AppError {
    AppError::ServiceUnavailable("카드 메모 암호화 저장소를 사용할 수 없습니다.".into())
}
impl CardMetadataKey {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let bytes = STANDARD.decode(value).map_err(|_| {
            anyhow::anyhow!("CARD_METADATA_KEY must be base64 encoding of 32 bytes")
        })?;
        anyhow::ensure!(
            bytes.len() == 32,
            "CARD_METADATA_KEY must encode exactly 32 bytes"
        );
        let cipher = Aes256Gcm::new_from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid CARD_METADATA_KEY length"))?;
        Ok(Self {
            cipher,
            id: hex::encode(Sha256::digest(&bytes)),
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    fn aad(&self, owner: Uuid, id: Uuid) -> String {
        format!("card-metadata:v1:{}:{owner}:{id}", self.id)
    }
    pub fn encrypt(
        &self,
        owner: Uuid,
        id: Uuid,
        input: &CardMetadata,
    ) -> Result<(Vec<u8>, Vec<u8>), AppError> {
        let plaintext = serde_json::to_vec(input).map_err(|_| unavailable())?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let aad = self.aad(owner, id);
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: &plaintext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| unavailable())?;
        Ok((nonce.to_vec(), ciphertext))
    }
    pub fn decrypt(
        &self,
        owner: Uuid,
        row: &crate::cards::StoredCard,
    ) -> Result<CardMetadata, AppError> {
        if row.format_version != 1 || row.key_id != self.id || row.nonce.len() != 12 {
            return Err(unavailable());
        }
        let aad = self.aad(owner, row.id);
        let plaintext = self
            .cipher
            .decrypt(
                Nonce::from_slice(&row.nonce),
                Payload {
                    msg: &row.ciphertext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| unavailable())?;
        serde_json::from_slice(&plaintext).map_err(|_| unavailable())
    }
}
