use std::path::Path;

use anyhow::Context;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

/// Claims embedded in per-agent JWTs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentClaims {
    /// Task ID
    pub sub: String,
    /// Skill name (e.g. "engineer", "researcher")
    pub role: String,
    /// Optional project ID
    pub project: Option<String>,
    /// Issued-at timestamp (unix seconds)
    pub iat: u64,
}

/// Signs and verifies per-agent JWTs using HS256.
///
/// The secret is persisted to disk so tokens survive dashboard restarts.
#[derive(Clone)]
pub struct JwtSigner {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
}

impl JwtSigner {
    /// Load the signing secret from `secret_path`, or generate and persist a new one.
    pub fn load_or_create(secret_path: &Path) -> anyhow::Result<Self> {
        let secret = if secret_path.exists() {
            std::fs::read(secret_path).context("failed to read JWT secret")?
        } else {
            use rand::RngCore;
            let mut key = vec![0u8; 32];
            rand::rng().fill_bytes(&mut key);
            if let Some(parent) = secret_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(secret_path, &key).context("failed to write JWT secret")?;
            key
        };

        Ok(Self {
            encoding_key: EncodingKey::from_secret(&secret),
            decoding_key: DecodingKey::from_secret(&secret),
        })
    }

    /// Sign an `AgentClaims` payload and return the compact JWT string.
    pub fn sign(&self, claims: &AgentClaims) -> anyhow::Result<String> {
        jsonwebtoken::encode(&Header::default(), claims, &self.encoding_key)
            .context("failed to sign JWT")
    }

    /// Verify a JWT string and return the decoded claims.
    pub fn verify(&self, token: &str) -> anyhow::Result<AgentClaims> {
        let mut validation = Validation::default();
        // No expiry on agent tokens.
        validation.required_spec_claims.clear();
        validation.validate_exp = false;
        let data = jsonwebtoken::decode::<AgentClaims>(token, &self.decoding_key, &validation)
            .context("invalid JWT")?;
        Ok(data.claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let signer = JwtSigner::load_or_create(&dir.path().join("secret")).unwrap();

        let claims = AgentClaims {
            sub: "task-123".into(),
            role: "engineer".into(),
            project: Some("my-project".into()),
            iat: 1_700_000_000,
        };

        let token = signer.sign(&claims).unwrap();
        let decoded = signer.verify(&token).unwrap();

        assert_eq!(decoded.sub, "task-123");
        assert_eq!(decoded.role, "engineer");
        assert_eq!(decoded.project.as_deref(), Some("my-project"));
        assert_eq!(decoded.iat, 1_700_000_000);
    }

    #[test]
    fn verify_rejects_tampered_token() {
        let dir = tempfile::tempdir().unwrap();
        let signer = JwtSigner::load_or_create(&dir.path().join("secret")).unwrap();

        let claims = AgentClaims {
            sub: "task-456".into(),
            role: "researcher".into(),
            project: None,
            iat: 1_700_000_000,
        };

        let mut token = signer.sign(&claims).unwrap();
        // Tamper with the payload
        token.push('x');

        assert!(signer.verify(&token).is_err());
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let dir = tempfile::tempdir().unwrap();
        let signer1 = JwtSigner::load_or_create(&dir.path().join("secret1")).unwrap();
        let signer2 = JwtSigner::load_or_create(&dir.path().join("secret2")).unwrap();

        let claims = AgentClaims {
            sub: "task-789".into(),
            role: "engineer".into(),
            project: None,
            iat: 1_700_000_000,
        };

        let token = signer1.sign(&claims).unwrap();
        assert!(signer2.verify(&token).is_err());
    }

    #[test]
    fn load_persists_and_reloads_key() {
        let dir = tempfile::tempdir().unwrap();
        let secret_path = dir.path().join("secret");

        let signer1 = JwtSigner::load_or_create(&secret_path).unwrap();
        let claims = AgentClaims {
            sub: "test".into(),
            role: "engineer".into(),
            project: None,
            iat: 0,
        };
        let token = signer1.sign(&claims).unwrap();

        // Reload from the same file — should be able to verify the token.
        let signer2 = JwtSigner::load_or_create(&secret_path).unwrap();
        let decoded = signer2.verify(&token).unwrap();
        assert_eq!(decoded.sub, "test");
    }
}
