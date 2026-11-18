use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Hash a password with argon2id and a random salt.
pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)?
        .to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// A new opaque bearer token. Only its SHA-256 is stored.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Snowflake-like IDs: milliseconds since the Steel epoch, shifted, plus randomness. Strings on the wire.
pub fn new_id() -> String {
    const STEEL_EPOCH_MS: u64 = 1_767_225_600_000; // 2026-01-01T00:00:00Z
    let now = chrono::Utc::now().timestamp_millis() as u64;
    let ts = now.saturating_sub(STEEL_EPOCH_MS);
    let noise = rand::thread_rng().next_u32() as u64 & 0x3F_FFFF;
    ((ts << 22) | noise).to_string()
}

/// A random numeric ID for attachments. Clients parse IDs as numbers, and the value is unguessable.
pub fn new_random_numeric_id() -> String {
    let n = rand::thread_rng().next_u64() & (i64::MAX as u64);
    n.max(1).to_string()
}
