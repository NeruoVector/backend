use super::*;
use crate::card_crypto::CardMetadataKey;
use base64::{Engine, engine::general_purpose::STANDARD};
fn key(byte: u8) -> CardMetadataKey {
    CardMetadataKey::parse(&STANDARD.encode([byte; 32])).unwrap()
}
fn input() -> CardMetadata {
    CardMetadata {
        kind: DemoKind::Card,
        nickname: "생활 카드".into(),
        last4: "0012".into(),
    }
}
fn fixture() -> (CardMetadataKey, Uuid, StoredCard) {
    let key = key(7);
    let owner = Uuid::new_v4();
    let id = Uuid::new_v4();
    let (nonce, ciphertext) = key.encrypt(owner, id, &input()).unwrap();
    let row = StoredCard {
        id,
        nonce,
        ciphertext,
        key_id: key.id().into(),
        format_version: 1,
        created_at: Utc::now(),
    };
    (key, owner, row)
}
#[test]
fn roundtrip_preserves_leading_zero_when_encrypted() {
    // Given
    let (key, owner, row) = fixture();
    // When
    let decoded = key.decrypt(owner, &row).unwrap();
    // Then
    assert_eq!(
        (decoded.nickname, decoded.last4),
        ("생활 카드".into(), "0012".into())
    );
}
#[test]
fn decryption_fails_when_ciphertext_tampered() {
    // Given
    let (key, owner, mut row) = fixture();
    row.ciphertext[0] ^= 1;
    // When
    let result = key.decrypt(owner, &row);
    // Then
    assert!(result.is_err());
}
#[test]
fn decryption_fails_when_owner_changed() {
    // Given
    let (key, _, row) = fixture();
    // When
    let result = key.decrypt(Uuid::new_v4(), &row);
    // Then
    assert!(result.is_err());
}
#[test]
fn decryption_fails_when_key_changed() {
    // Given
    let (_, owner, row) = fixture();
    // When
    let result = key(8).decrypt(owner, &row);
    // Then
    assert!(result.is_err());
}
#[test]
fn decryption_fails_when_record_changed() {
    // Given
    let (key, owner, mut row) = fixture();
    row.id = Uuid::new_v4();
    // When
    let result = key.decrypt(owner, &row);
    // Then
    assert!(result.is_err());
}
#[test]
fn encryption_uses_fresh_nonce_when_plaintext_repeated() {
    // Given
    let (key, owner, row) = fixture();
    // When
    let (nonce, _) = key.encrypt(owner, row.id, &input()).unwrap();
    // Then
    assert_ne!(nonce, row.nonce);
}
#[test]
fn generation_returns_maskable_demo_metadata_for_each_kind() {
    // Given
    let kinds = [DemoKind::Card, DemoKind::BankAccount, DemoKind::Mobile];
    // When
    let generated: Vec<_> = kinds.into_iter().map(CardMetadata::generate).collect();
    // Then
    assert!(generated.into_iter().all(|result| {
        let metadata = result.unwrap();
        metadata.last4.len() == 4 && metadata.last4.bytes().all(|b| b.is_ascii_digit())
    }));
}
#[test]
fn request_rejects_unknown_kind() {
    // Given
    let body = r#"{"kind":"real_card"}"#;
    // When
    let result = serde_json::from_str::<CardInput>(body);
    // Then
    assert!(result.is_err());
}
#[test]
fn request_rejects_unknown_sensitive_fields() {
    // Given
    let body = r#"{"kind":"card","pan":"4111111111111111"}"#;
    // When
    let result = serde_json::from_str::<CardInput>(body);
    // Then
    assert!(result.is_err());
}
#[test]
fn key_parse_rejects_wrong_lengths_and_encoding() {
    // Given
    let cases = [
        "invalid!".to_owned(),
        STANDARD.encode([1; 31]),
        STANDARD.encode([1; 33]),
    ];
    // When
    let results: Vec<_> = cases.iter().map(|v| CardMetadataKey::parse(v)).collect();
    // Then
    assert!(results.into_iter().all(|r| r.is_err()));
}
#[test]
fn decryption_fails_when_format_is_unknown() {
    // Given
    let (key, owner, mut row) = fixture();
    row.format_version = 2;
    // When
    let result = key.decrypt(owner, &row);
    // Then
    assert!(result.is_err());
}
#[test]
fn decryption_fails_when_nonce_length_is_invalid() {
    // Given
    let (key, owner, mut row) = fixture();
    row.nonce.pop();
    // When
    let result = key.decrypt(owner, &row);
    // Then
    assert!(result.is_err());
}
