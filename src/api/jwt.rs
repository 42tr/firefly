use hmac::{Hmac, Mac};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::config;

const SESSION_AUDIENCE: &str = "firefly-local-session-v1";

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub aud: String,
    pub exp: i64,
    pub iat: i64,
}

impl Claims {
    pub fn new(expiry_hours: i64) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            aud: SESSION_AUDIENCE.into(),
            exp: (now + Duration::hours(expiry_hours)).unix_timestamp(),
            iat: now.unix_timestamp(),
        }
    }
}

#[derive(Debug, Error)]
pub enum JwtError {
    #[error("JWT error: {0}")]
    General(#[from] jsonwebtoken::errors::Error),
}

// Changing the access password invalidates previously issued sessions as well.
fn session_key(cfg: &config::Config) -> Vec<u8> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(cfg.jwt_secret.as_bytes()).expect("HMAC accepts any key");
    mac.update(SESSION_AUDIENCE.as_bytes());
    mac.update(cfg.auth_password.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

pub fn create(claims: &Claims) -> Result<String, JwtError> {
    Ok(encode(
        &Header::default(),
        claims,
        &EncodingKey::from_secret(&session_key(&config::get())),
    )?)
}

pub fn validate(token: &str) -> Result<Claims, JwtError> {
    validate_with_config(token, &config::get())
}

fn validate_with_config(token: &str, cfg: &config::Config) -> Result<Claims, JwtError> {
    let mut validation = Validation::default();
    validation.set_audience(&[SESSION_AUDIENCE]);
    validation.leeway = 0;
    Ok(decode::<Claims>(
        token,
        &DecodingKey::from_secret(&session_key(cfg)),
        &validation,
    )?
    .claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_sessions_expire_and_reject_old_or_changed_credentials() {
        config::init_test_config();
        let token = create(&Claims::new(1)).unwrap();
        assert!(validate(&token).is_ok());
        assert!(validate(&create(&Claims::new(-1)).unwrap()).is_err());
        let old = encode(
            &Header::default(),
            &serde_json::json!({"sub":"external-owner", "exp": Claims::new(1).exp}),
            &EncodingKey::from_secret(config::get().jwt_secret.as_bytes()),
        )
        .unwrap();
        assert!(validate(&old).is_err());
        let mut changed: config::Config = serde_json::from_value(serde_json::json!({"jwt_secret":config::get().jwt_secret, "auth_password":"a-different-password"})).unwrap();
        assert!(validate_with_config(&token, &changed).is_err());
        changed.auth_password = config::get().auth_password.clone();
        changed.jwt_secret = "another-secret".into();
        assert!(validate_with_config(&token, &changed).is_err());
    }
}
