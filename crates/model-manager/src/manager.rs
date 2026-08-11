use crate::manifest::{Artifact, ModelGroup, PostInstall, SignedManifest};
use fs2::FileExt;
use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, HeaderValue, RANGE};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use url::Url;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelState {
    Missing,
    Partial,
    Ready,
    Corrupt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InstallSource {
    Managed,
    Legacy,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub id: String,
    pub display_name: String,
    pub state: ModelState,
    pub source: Option<InstallSource>,
    pub verified_bytes: u64,
    pub total_bytes: u64,
    pub license_name: String,
    pub license_url: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallProgress {
    pub model_id: String,
    pub artifact: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallReceipt {
    release: String,
    model_id: String,
    artifacts: Vec<ReceiptArtifact>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReceiptArtifact {
    path: String,
    size_bytes: u64,
    sha256: String,
    modified_nanos: u128,
}

#[derive(Debug, Error)]
pub enum ModelManagerError {
    #[error(transparent)]
    Manifest(#[from] crate::manifest::ManifestError),
    #[error("model group is unknown: {0}")]
    UnknownModel(String),
    #[error("model manager is already modifying this store")]
    Locked,
    #[error("model artifact I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("model download failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("model download returned HTTP {status} for {url}")]
    HttpStatus { status: StatusCode, url: String },
    #[error("model artifact {path} has size {actual}, expected {expected}")]
    Size {
        path: PathBuf,
        actual: u64,
        expected: u64,
    },
    #[error("model artifact {path} failed SHA-256 verification")]
    Checksum { path: PathBuf },
    #[error("model store does not have enough free space: need {required} bytes, have {available}")]
    DiskSpace { required: u64, available: u64 },
    #[error("model URL cannot be constructed: {0}")]
    Url(String),
    #[error("post-install extraction failed for {path}: {message}")]
    Extract { path: PathBuf, message: String },
    #[error("system clock is before the Unix epoch")]
    Clock,
}

pub struct ModelManager {
    workspace_root: PathBuf,
    store_root: PathBuf,
    manifest: SignedManifest,
    client: reqwest::Client,
    hub_base_url: Url,
}

impl ModelManager {
    pub fn new(
        workspace_root: impl Into<PathBuf>,
        store_root: impl Into<PathBuf>,
        manifest: SignedManifest,
    ) -> Result<Self, ModelManagerError> {
        let client = reqwest::Client::builder()
            .user_agent("FastTalk/0.1 model-manager")
            .build()?;
        Ok(Self {
            workspace_root: workspace_root.into(),
            store_root: store_root.into(),
            manifest,
            client,
            hub_base_url: Url::parse("https://huggingface.co/")
                .expect("the fixed Hugging Face base URL is valid"),
        })
    }

    #[must_use]
    pub fn manifest(&self) -> &SignedManifest {
        &self.manifest
    }

    pub fn statuses(&self) -> Vec<ModelStatus> {
        self.manifest
            .manifest()
            .models
            .iter()
            .map(|model| self.status_for(model))
            .collect()
    }

    pub fn resolved_root(&self, id: &str) -> Result<Option<PathBuf>, ModelManagerError> {
        let model = self.model(id)?;
        let managed = self.managed_root(model);
        if self.verify_managed_group(model).is_ok() {
            return Ok(Some(managed));
        }
        let legacy = self.workspace_root.join(&model.legacy_root);
        if self.verify_legacy_group(model, &legacy).is_ok() {
            return Ok(Some(legacy));
        }
        Ok(None)
    }

    pub async fn install_all(
        &self,
        token: Option<&str>,
        progress: &(dyn Fn(InstallProgress) + Send + Sync),
    ) -> Result<Vec<ModelStatus>, ModelManagerError> {
        let _lock = self.lock_store()?;
        self.check_disk_space()?;
        for model in &self.manifest.manifest().models {
            if self.resolved_root(&model.id)?.is_some() {
                continue;
            }
            self.install_group(model, token, progress).await?;
        }
        Ok(self.statuses())
    }

    pub(crate) fn model(&self, id: &str) -> Result<&ModelGroup, ModelManagerError> {
        self.manifest
            .manifest()
            .models
            .iter()
            .find(|model| model.id == id)
            .ok_or_else(|| ModelManagerError::UnknownModel(id.to_owned()))
    }

    pub(crate) fn managed_root(&self, model: &ModelGroup) -> PathBuf {
        self.store_root
            .join("versions")
            .join(&self.manifest.manifest().release)
            .join(&model.id)
    }

    pub(crate) fn store_root(&self) -> &Path {
        &self.store_root
    }

    pub(crate) fn lock_store(&self) -> Result<std::fs::File, ModelManagerError> {
        create_dir_all(&self.store_root)?;
        let path = self.store_root.join("install.lock");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| io_error(&path, source))?;
        file.try_lock_exclusive()
            .map_err(|_| ModelManagerError::Locked)?;
        Ok(file)
    }

    fn status_for(&self, model: &ModelGroup) -> ModelStatus {
        let total_bytes = model
            .artifacts
            .iter()
            .map(|artifact| artifact.size_bytes)
            .sum();
        let managed = self.managed_root(model);
        if self.verify_managed_group(model).is_ok() {
            return ready_status(model, total_bytes, InstallSource::Managed);
        }
        let legacy = self.workspace_root.join(&model.legacy_root);
        if self.verify_legacy_group(model, &legacy).is_ok() {
            return ready_status(model, total_bytes, InstallSource::Legacy);
        }
        let staging = self.staging_root(model);
        let verified_bytes = model
            .artifacts
            .iter()
            .filter_map(|artifact| {
                let final_path = staging.join(&artifact.path);
                if verify_artifact(artifact, &final_path).is_ok() {
                    Some(artifact.size_bytes)
                } else {
                    let partial = partial_path(&final_path);
                    std::fs::metadata(partial)
                        .ok()
                        .map(|metadata| metadata.len())
                }
            })
            .sum();
        let managed_exists = managed.exists() || legacy.exists();
        let state = if verified_bytes > 0 {
            ModelState::Partial
        } else if managed_exists {
            ModelState::Corrupt
        } else {
            ModelState::Missing
        };
        ModelStatus {
            id: model.id.clone(),
            display_name: model.display_name.clone(),
            state,
            source: None,
            verified_bytes,
            total_bytes,
            license_name: model.license.name.clone(),
            license_url: model.license.url.clone(),
            error: (state == ModelState::Corrupt)
                .then(|| "one or more installed files failed verification".to_owned()),
        }
    }

    fn staging_root(&self, model: &ModelGroup) -> PathBuf {
        self.store_root
            .join("staging")
            .join(&self.manifest.manifest().release)
            .join(&model.id)
    }

    fn check_disk_space(&self) -> Result<(), ModelManagerError> {
        create_dir_all(&self.store_root)?;
        let required: u64 = self
            .manifest
            .manifest()
            .models
            .iter()
            .filter(|model| self.resolved_root(&model.id).ok().flatten().is_none())
            .flat_map(|model| &model.artifacts)
            .map(|artifact| artifact.size_bytes)
            .sum();
        let available = fs2::available_space(&self.store_root)
            .map_err(|source| io_error(&self.store_root, source))?;
        if required > available {
            return Err(ModelManagerError::DiskSpace {
                required,
                available,
            });
        }
        Ok(())
    }

    async fn install_group(
        &self,
        model: &ModelGroup,
        token: Option<&str>,
        progress: &(dyn Fn(InstallProgress) + Send + Sync),
    ) -> Result<(), ModelManagerError> {
        let staging = self.staging_root(model);
        create_dir_all(&staging)?;
        for artifact in &model.artifacts {
            let destination = staging.join(&artifact.path);
            if verify_artifact(artifact, &destination).is_ok() {
                continue;
            }
            self.download_artifact(model, artifact, &destination, token, progress)
                .await?;
        }
        run_post_install(model, &staging)?;
        verify_group(model, &staging)?;
        self.write_receipt(model, &staging)?;
        self.activate(model, &staging)
    }

    async fn download_artifact(
        &self,
        model: &ModelGroup,
        artifact: &Artifact,
        destination: &Path,
        token: Option<&str>,
        progress: &(dyn Fn(InstallProgress) + Send + Sync),
    ) -> Result<(), ModelManagerError> {
        if let Some(parent) = destination.parent() {
            create_dir_all(parent)?;
        }
        let partial = partial_path(destination);
        let mut offset = std::fs::metadata(&partial)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if offset > artifact.size_bytes {
            remove_file(&partial)?;
            offset = 0;
        }
        let url = hub_url(&self.hub_base_url, model, artifact)?;
        let mut request = self.client.get(url.clone());
        if offset > 0 {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        if let Some(token) = token.filter(|token| !token.trim().is_empty()) {
            let header = HeaderValue::from_str(&format!("Bearer {}", token.trim()))
                .map_err(|error| ModelManagerError::Url(error.to_string()))?;
            request = request.header(AUTHORIZATION, header);
        }
        let response = request.send().await?;
        if offset > 0 && response.status() == StatusCode::RANGE_NOT_SATISFIABLE {
            verify_artifact(artifact, &partial)?;
            rename_file(&partial, destination)?;
            return Ok(());
        }
        if !response.status().is_success() {
            return Err(ModelManagerError::HttpStatus {
                status: response.status(),
                url: url.to_string(),
            });
        }
        let append = offset > 0 && response.status() == StatusCode::PARTIAL_CONTENT;
        if offset > 0 && !append {
            remove_file(&partial)?;
            offset = 0;
        }
        let mut output = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(&partial)
            .await
            .map_err(|source| io_error(&partial, source))?;
        let mut downloaded = offset;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            output
                .write_all(&chunk)
                .await
                .map_err(|source| io_error(&partial, source))?;
            downloaded += chunk.len() as u64;
            progress(InstallProgress {
                model_id: model.id.clone(),
                artifact: artifact.path.clone(),
                downloaded_bytes: downloaded.min(artifact.size_bytes),
                total_bytes: artifact.size_bytes,
            });
        }
        output
            .flush()
            .await
            .map_err(|source| io_error(&partial, source))?;
        drop(output);
        verify_artifact(artifact, &partial)?;
        rename_file(&partial, destination)
    }

    pub(crate) fn activate(
        &self,
        model: &ModelGroup,
        staging: &Path,
    ) -> Result<(), ModelManagerError> {
        let managed = self.managed_root(model);
        if let Some(parent) = managed.parent() {
            create_dir_all(parent)?;
        }
        if managed.exists() {
            let quarantine = self.store_root.join("quarantine").join(format!(
                "{}-{}",
                model.id,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| ModelManagerError::Clock)?
                    .as_secs()
            ));
            if let Some(parent) = quarantine.parent() {
                create_dir_all(parent)?;
            }
            std::fs::rename(&managed, &quarantine).map_err(|source| io_error(&managed, source))?;
        }
        std::fs::rename(staging, &managed).map_err(|source| io_error(staging, source))?;
        Ok(())
    }

    pub(crate) fn write_receipt(
        &self,
        model: &ModelGroup,
        root: &Path,
    ) -> Result<(), ModelManagerError> {
        let artifacts = model
            .artifacts
            .iter()
            .map(|artifact| {
                let path = root.join(&artifact.path);
                let metadata =
                    std::fs::symlink_metadata(&path).map_err(|source| io_error(&path, source))?;
                if !metadata.file_type().is_file() {
                    return Err(ModelManagerError::Io {
                        path,
                        source: std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "model artifact must be a regular file",
                        ),
                    });
                }
                Ok(ReceiptArtifact {
                    path: artifact.path.clone(),
                    size_bytes: artifact.size_bytes,
                    sha256: artifact.sha256.clone(),
                    modified_nanos: modified_nanos(&metadata, &path)?,
                })
            })
            .collect::<Result<Vec<_>, ModelManagerError>>()?;
        let receipt = InstallReceipt {
            release: self.manifest.manifest().release.clone(),
            model_id: model.id.clone(),
            artifacts,
        };
        let path = root.join(".fasttalk-install.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&receipt).expect("receipt serializes"),
        )
        .map_err(|source| io_error(&path, source))
    }

    fn verify_managed_group(&self, model: &ModelGroup) -> Result<(), ModelManagerError> {
        let root = self.managed_root(model);
        self.verify_receipted_group(model, &root)
    }

    fn verify_legacy_group(
        &self,
        model: &ModelGroup,
        root: &Path,
    ) -> Result<(), ModelManagerError> {
        if self.verify_receipted_group(model, root).is_ok() {
            return Ok(());
        }
        verify_group(model, root)?;
        // A legacy checkout may be read-only. Verification is still valid for
        // this run, but writable caches get a receipt so future checks only
        // need file metadata instead of hashing model weights again.
        let _ = self.write_receipt(model, root);
        Ok(())
    }

    fn verify_receipted_group(
        &self,
        model: &ModelGroup,
        root: &Path,
    ) -> Result<(), ModelManagerError> {
        let path = root.join(".fasttalk-install.json");
        let bytes = std::fs::read(&path).map_err(|source| io_error(&path, source))?;
        let receipt: InstallReceipt =
            serde_json::from_slice(&bytes).map_err(|error| ModelManagerError::Extract {
                path: path.clone(),
                message: error.to_string(),
            })?;
        if receipt.release != self.manifest.manifest().release
            || receipt.model_id != model.id
            || receipt.artifacts.len() != model.artifacts.len()
        {
            return Err(ModelManagerError::Checksum { path });
        }
        for (artifact, recorded) in model.artifacts.iter().zip(receipt.artifacts.iter()) {
            let artifact_path = root.join(&artifact.path);
            let metadata = std::fs::symlink_metadata(&artifact_path)
                .map_err(|source| io_error(&artifact_path, source))?;
            if recorded.path != artifact.path
                || recorded.size_bytes != artifact.size_bytes
                || recorded.sha256 != artifact.sha256
                || !metadata.file_type().is_file()
                || metadata.len() != artifact.size_bytes
                || modified_nanos(&metadata, &artifact_path)? != recorded.modified_nanos
            {
                return Err(ModelManagerError::Checksum {
                    path: artifact_path,
                });
            }
        }
        verify_extraction_markers(model, &root)
    }
}

fn ready_status(model: &ModelGroup, total_bytes: u64, source: InstallSource) -> ModelStatus {
    ModelStatus {
        id: model.id.clone(),
        display_name: model.display_name.clone(),
        state: ModelState::Ready,
        source: Some(source),
        verified_bytes: total_bytes,
        total_bytes,
        license_name: model.license.name.clone(),
        license_url: model.license.url.clone(),
        error: None,
    }
}

pub(crate) fn verify_group(model: &ModelGroup, root: &Path) -> Result<(), ModelManagerError> {
    for artifact in &model.artifacts {
        verify_artifact(artifact, &root.join(&artifact.path))?;
    }
    verify_extraction_markers(model, root)
}

fn verify_extraction_markers(model: &ModelGroup, root: &Path) -> Result<(), ModelManagerError> {
    for action in &model.post_install {
        match action {
            PostInstall::ExtractTar { destination, .. } => {
                let marker = root.join(destination).join(".fasttalk-verified");
                if !marker.is_file() {
                    return Err(ModelManagerError::Io {
                        path: marker,
                        source: std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "verified extraction marker is missing",
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

fn modified_nanos(metadata: &std::fs::Metadata, path: &Path) -> Result<u128, ModelManagerError> {
    metadata
        .modified()
        .map_err(|source| io_error(path, source))?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ModelManagerError::Clock)
        .map(|duration| duration.as_nanos())
}

pub(crate) fn verify_artifact(artifact: &Artifact, path: &Path) -> Result<(), ModelManagerError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if !metadata.file_type().is_file() {
        return Err(ModelManagerError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "model artifact must be a regular file",
            ),
        });
    }
    if metadata.len() != artifact.size_bytes {
        return Err(ModelManagerError::Size {
            path: path.to_path_buf(),
            actual: metadata.len(),
            expected: artifact.size_bytes,
        });
    }
    let mut file = std::fs::File::open(path).map_err(|source| io_error(path, source))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| io_error(path, source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if hex::encode(hasher.finalize()) != artifact.sha256.to_ascii_lowercase() {
        return Err(ModelManagerError::Checksum {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

pub(crate) fn run_post_install(model: &ModelGroup, root: &Path) -> Result<(), ModelManagerError> {
    for action in &model.post_install {
        match action {
            PostInstall::ExtractTar {
                artifact,
                destination,
            } => {
                let archive_path = root.join(artifact);
                let destination = root.join(destination);
                if destination.exists() {
                    std::fs::remove_dir_all(&destination)
                        .map_err(|source| io_error(&destination, source))?;
                }
                create_dir_all(&destination)?;
                let file = std::fs::File::open(&archive_path)
                    .map_err(|source| io_error(&archive_path, source))?;
                let mut archive = tar::Archive::new(file);
                archive
                    .unpack(&destination)
                    .map_err(|error| ModelManagerError::Extract {
                        path: archive_path.clone(),
                        message: error.to_string(),
                    })?;
                let marker = destination.join(".fasttalk-verified");
                let mut marker_file =
                    std::fs::File::create(&marker).map_err(|source| io_error(&marker, source))?;
                marker_file
                    .write_all(b"verified\n")
                    .map_err(|source| io_error(&marker, source))?;
            }
        }
    }
    Ok(())
}

fn hub_url(
    base_url: &Url,
    model: &ModelGroup,
    artifact: &Artifact,
) -> Result<Url, ModelManagerError> {
    let mut url = base_url.clone();
    {
        let mut segments = url.path_segments_mut().map_err(|_| {
            ModelManagerError::Url("Hugging Face URL is not hierarchical".to_owned())
        })?;
        for segment in model.repository.split('/') {
            segments.push(segment);
        }
        segments.push("resolve");
        segments.push(&model.revision);
        for segment in artifact.remote_path.split('/') {
            segments.push(segment);
        }
    }
    url.query_pairs_mut().append_pair("download", "true");
    Ok(url)
}

fn partial_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(".partial");
    PathBuf::from(value)
}

fn create_dir_all(path: &Path) -> Result<(), ModelManagerError> {
    std::fs::create_dir_all(path).map_err(|source| io_error(path, source))
}

fn remove_file(path: &Path) -> Result<(), ModelManagerError> {
    if path.exists() {
        std::fs::remove_file(path).map_err(|source| io_error(path, source))?;
    }
    Ok(())
}

fn rename_file(source: &Path, destination: &Path) -> Result<(), ModelManagerError> {
    if destination.exists() {
        std::fs::remove_file(destination).map_err(|error| io_error(destination, error))?;
    }
    std::fs::rename(source, destination).map_err(|error| io_error(source, error))
}

fn io_error(path: &Path, source: std::io::Error) -> ModelManagerError {
    ModelManagerError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{LicenseNotice, ModelManifest};
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::{Signer, SigningKey};
    use std::net::TcpListener;
    use tempfile::TempDir;

    fn manager_with_artifact(temp: &TempDir, content: &[u8]) -> ModelManager {
        let manifest = ModelManifest {
            schema_version: 1,
            release: "test".to_owned(),
            public_key_id: "test".to_owned(),
            models: vec![ModelGroup {
                id: "fixture".to_owned(),
                display_name: "Fixture".to_owned(),
                repository: "owner/repo".to_owned(),
                revision: "0123456789012345678901234567890123456789".to_owned(),
                legacy_root: "legacy/fixture".to_owned(),
                artifacts: vec![Artifact {
                    remote_path: "model.bin".to_owned(),
                    path: "model.bin".to_owned(),
                    size_bytes: content.len() as u64,
                    sha256: hex::encode(Sha256::digest(content)),
                }],
                license: LicenseNotice {
                    id: "test".to_owned(),
                    name: "Test".to_owned(),
                    url: "https://example.invalid".to_owned(),
                },
                post_install: Vec::new(),
            }],
        };
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let key = SigningKey::from_bytes(&[3; 32]);
        let signature = STANDARD.encode(key.sign(&bytes).to_bytes());
        let public_key = STANDARD.encode(key.verifying_key().to_bytes());
        let signed = SignedManifest::verify(bytes, signature, &public_key).unwrap();
        ModelManager::new(temp.path(), temp.path().join("store"), signed).unwrap()
    }

    #[test]
    fn recognizes_verified_legacy_and_rejects_corruption() {
        let temp = TempDir::new().unwrap();
        let manager = manager_with_artifact(&temp, b"fixture");
        let legacy = temp.path().join("legacy/fixture/model.bin");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"fixture").unwrap();
        let status = manager.statuses().remove(0);
        assert_eq!(status.state, ModelState::Ready);
        assert_eq!(status.source, Some(InstallSource::Legacy));
        assert!(
            legacy
                .parent()
                .unwrap()
                .join(".fasttalk-install.json")
                .is_file()
        );

        std::fs::write(&legacy, b"corrupt").unwrap();
        let status = manager.statuses().remove(0);
        assert_eq!(status.state, ModelState::Corrupt);
    }

    #[test]
    fn reports_partial_download_bytes() {
        let temp = TempDir::new().unwrap();
        let manager = manager_with_artifact(&temp, b"fixture");
        let model = manager.model("fixture").unwrap();
        let partial = partial_path(&manager.staging_root(model).join("model.bin"));
        std::fs::create_dir_all(partial.parent().unwrap()).unwrap();
        std::fs::write(partial, b"fix").unwrap();
        let status = manager.statuses().remove(0);
        assert_eq!(status.state, ModelState::Partial);
        assert_eq!(status.verified_bytes, 3);
    }

    #[tokio::test]
    async fn resumes_a_partial_download_and_activates_atomically() {
        let temp = TempDir::new().unwrap();
        let mut manager = manager_with_artifact(&temp, b"fixture");
        let model = manager.model("fixture").unwrap();
        let partial = partial_path(&manager.staging_root(model).join("model.bin"));
        std::fs::create_dir_all(partial.parent().unwrap()).unwrap();
        std::fs::write(&partial, b"fix").unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        manager.hub_base_url = Url::parse(&format!("http://{address}/")).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.contains("Range: bytes=3-") || request.contains("range: bytes=3-"));
            stream
                .write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 4\r\nConnection: close\r\n\r\nture")
                .unwrap();
        });

        manager.install_all(None, &|_| {}).await.unwrap();
        server.join().unwrap();
        let status = manager.statuses().remove(0);
        assert_eq!(status.state, ModelState::Ready);
        assert_eq!(status.source, Some(InstallSource::Managed));
        assert_eq!(
            std::fs::read(
                manager
                    .managed_root(manager.model("fixture").unwrap())
                    .join("model.bin")
            )
            .unwrap(),
            b"fixture"
        );
    }

    #[test]
    fn exports_and_imports_a_verified_offline_pack() {
        let source = TempDir::new().unwrap();
        let manager = manager_with_artifact(&source, b"fixture");
        let legacy = source.path().join("legacy/fixture/model.bin");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"fixture").unwrap();
        let pack = source.path().join("models.tar");
        manager.export_pack(&pack).unwrap();

        let destination = TempDir::new().unwrap();
        let imported = ModelManager::new(
            destination.path(),
            destination.path().join("store"),
            manager.manifest.clone(),
        )
        .unwrap();
        let statuses = imported.import_pack(&pack).unwrap();
        assert_eq!(statuses[0].state, ModelState::Ready);
        assert_eq!(statuses[0].source, Some(InstallSource::Managed));

        let installed = imported
            .managed_root(imported.model("fixture").unwrap())
            .join("model.bin");
        std::fs::write(installed, b"bad").unwrap();
        assert_eq!(imported.statuses()[0].state, ModelState::Corrupt);
    }
}
