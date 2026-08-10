use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Component, Path};
use thiserror::Error;

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelManifest {
    pub schema_version: u32,
    pub release: String,
    pub public_key_id: String,
    pub models: Vec<ModelGroup>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelGroup {
    pub id: String,
    pub display_name: String,
    pub repository: String,
    pub revision: String,
    pub legacy_root: String,
    pub artifacts: Vec<Artifact>,
    pub license: LicenseNotice,
    #[serde(default)]
    pub post_install: Vec<PostInstall>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub remote_path: String,
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseNotice {
    pub id: String,
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PostInstall {
    ExtractTar {
        artifact: String,
        destination: String,
    },
}

#[derive(Clone, Debug)]
pub struct SignedManifest {
    bytes: Vec<u8>,
    signature: String,
    manifest: ModelManifest,
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("model manifest public key must be 32 bytes")]
    PublicKeyLength,
    #[error("model manifest signature must be 64 bytes")]
    SignatureLength,
    #[error("model manifest base64 is invalid: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("model manifest public key is invalid")]
    PublicKey,
    #[error("model manifest signature is not trusted")]
    UntrustedSignature,
    #[error("model manifest JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("model manifest is invalid: {0}")]
    Invalid(String),
}

impl SignedManifest {
    pub fn verify(
        bytes: impl Into<Vec<u8>>,
        signature_base64: impl Into<String>,
        public_key_base64: &str,
    ) -> Result<Self, ManifestError> {
        let bytes = bytes.into();
        let signature = signature_base64.into();
        let public_key_bytes = STANDARD.decode(public_key_base64.trim())?;
        let public_key_bytes: [u8; 32] = public_key_bytes
            .try_into()
            .map_err(|_| ManifestError::PublicKeyLength)?;
        let verifying_key =
            VerifyingKey::from_bytes(&public_key_bytes).map_err(|_| ManifestError::PublicKey)?;
        let signature_bytes = STANDARD.decode(signature.trim())?;
        let signature_bytes: [u8; 64] = signature_bytes
            .try_into()
            .map_err(|_| ManifestError::SignatureLength)?;
        let signature_value = Signature::from_bytes(&signature_bytes);
        verifying_key
            .verify(&bytes, &signature_value)
            .map_err(|_| ManifestError::UntrustedSignature)?;
        let manifest: ModelManifest = serde_json::from_slice(&bytes)?;
        validate_manifest(&manifest)?;
        Ok(Self {
            bytes,
            signature,
            manifest,
        })
    }

    #[must_use]
    pub fn manifest(&self) -> &ModelManifest {
        &self.manifest
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn signature(&self) -> &str {
        &self.signature
    }
}

fn validate_manifest(manifest: &ModelManifest) -> Result<(), ManifestError> {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        return Err(ManifestError::Invalid(format!(
            "unsupported schema version {}",
            manifest.schema_version
        )));
    }
    if manifest.release.trim().is_empty() || manifest.public_key_id.trim().is_empty() {
        return Err(ManifestError::Invalid(
            "release and publicKeyId are required".to_owned(),
        ));
    }
    if manifest.models.is_empty() {
        return Err(ManifestError::Invalid(
            "at least one model group is required".to_owned(),
        ));
    }
    let mut ids = HashSet::new();
    for model in &manifest.models {
        if model.id.trim().is_empty() || !ids.insert(&model.id) {
            return Err(ManifestError::Invalid(format!(
                "model id is empty or duplicated: {}",
                model.id
            )));
        }
        validate_relative_path(&model.legacy_root)?;
        if model.repository.split('/').count() != 2 || model.revision.len() != 40 {
            return Err(ManifestError::Invalid(format!(
                "model {} does not pin a repository and 40-character revision",
                model.id
            )));
        }
        if model.artifacts.is_empty() {
            return Err(ManifestError::Invalid(format!(
                "model {} has no artifacts",
                model.id
            )));
        }
        let mut paths = HashSet::new();
        for artifact in &model.artifacts {
            validate_relative_path(&artifact.path)?;
            validate_relative_path(&artifact.remote_path)?;
            if artifact.size_bytes == 0
                || artifact.sha256.len() != 64
                || hex::decode(&artifact.sha256).is_err()
                || !paths.insert(&artifact.path)
            {
                return Err(ManifestError::Invalid(format!(
                    "model {} has an invalid or duplicate artifact {}",
                    model.id, artifact.path
                )));
            }
        }
        for action in &model.post_install {
            match action {
                PostInstall::ExtractTar {
                    artifact,
                    destination,
                } => {
                    validate_relative_path(artifact)?;
                    validate_relative_path(destination)?;
                    if !paths.contains(artifact) {
                        return Err(ManifestError::Invalid(format!(
                            "model {} extracts an unknown artifact {}",
                            model.id, artifact
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_relative_path(value: &str) -> Result<(), ManifestError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ManifestError::Invalid(format!(
            "path must contain only relative normal components: {value}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed_manifest(bytes: &[u8]) -> (String, String) {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let signature = signing.sign(bytes);
        (
            STANDARD.encode(signature.to_bytes()),
            STANDARD.encode(signing.verifying_key().to_bytes()),
        )
    }

    #[test]
    fn verifies_signature_before_parsing_manifest() {
        let bytes = br#"{"schemaVersion":1,"release":"test","publicKeyId":"test","models":[{"id":"m","displayName":"M","repository":"owner/repo","revision":"0123456789012345678901234567890123456789","legacyRoot":"models/m","artifacts":[{"remotePath":"model.bin","path":"model.bin","sizeBytes":1,"sha256":"0000000000000000000000000000000000000000000000000000000000000000"}],"license":{"id":"apache-2.0","name":"Apache 2.0","url":"https://example.invalid/license"},"postInstall":[]}]}"#;
        let (signature, key) = signed_manifest(bytes);
        let verified = SignedManifest::verify(bytes.to_vec(), signature, &key).unwrap();
        assert_eq!(verified.manifest().models[0].id, "m");
    }

    #[test]
    fn rejects_tampering_and_parent_paths() {
        let invalid = br#"{"schemaVersion":1,"release":"test","publicKeyId":"test","models":[{"id":"m","displayName":"M","repository":"owner/repo","revision":"0123456789012345678901234567890123456789","legacyRoot":"../models","artifacts":[{"remotePath":"model.bin","path":"model.bin","sizeBytes":1,"sha256":"0000000000000000000000000000000000000000000000000000000000000000"}],"license":{"id":"apache-2.0","name":"Apache 2.0","url":"https://example.invalid/license"}}]}"#;
        let (signature, key) = signed_manifest(invalid);
        assert!(matches!(
            SignedManifest::verify(invalid.to_vec(), signature.clone(), &key),
            Err(ManifestError::Invalid(_))
        ));

        let mut valid = invalid.to_vec();
        valid[1] ^= 1;
        assert!(matches!(
            SignedManifest::verify(valid, signature, &key),
            Err(ManifestError::UntrustedSignature)
        ));
    }
}
