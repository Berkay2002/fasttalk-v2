use crate::manager::{
    ModelManager, ModelManagerError, run_post_install, verify_artifact, verify_group,
};
use std::fs::File;
use std::io::Cursor;
use std::path::{Path, PathBuf};

const PACK_ROOT: &str = "fasttalk-model-pack";

impl ModelManager {
    pub fn export_pack(&self, output: &Path) -> Result<(), ModelManagerError> {
        let ids = self
            .manifest()
            .manifest()
            .models
            .iter()
            .map(|model| model.id.clone())
            .collect::<Vec<_>>();
        self.export_pack_groups(&ids, output)
    }

    pub fn export_pack_groups(
        &self,
        ids: &[String],
        output: &Path,
    ) -> Result<(), ModelManagerError> {
        let models = ids
            .iter()
            .map(|id| self.model(id).cloned())
            .collect::<Result<Vec<_>, _>>()?;
        let _lock = self.lock_store()?;
        let file = File::create(output).map_err(|source| pack_io(output, source))?;
        let mut archive = tar::Builder::new(file);
        append_bytes(
            &mut archive,
            &format!("{PACK_ROOT}/manifest.json"),
            self.manifest().bytes(),
        )?;
        append_bytes(
            &mut archive,
            &format!("{PACK_ROOT}/manifest.sig"),
            self.manifest().signature().as_bytes(),
        )?;
        for model in &models {
            let source = self.resolved_root(&model.id)?.ok_or_else(|| {
                ModelManagerError::UnknownModel(format!("{} is not installed", model.id))
            })?;
            for artifact in &model.artifacts {
                let path = source.join(&artifact.path);
                verify_artifact(artifact, &path)?;
                archive
                    .append_path_with_name(
                        &path,
                        format!("{PACK_ROOT}/models/{}/{}", model.id, artifact.path),
                    )
                    .map_err(|source| pack_io(&path, source))?;
            }
        }
        archive.finish().map_err(|source| pack_io(output, source))?;
        Ok(())
    }

    pub fn import_pack(
        &self,
        pack: &Path,
    ) -> Result<Vec<crate::manager::ModelStatus>, ModelManagerError> {
        let _lock = self.lock_store()?;
        let staging = self
            .manifest()
            .manifest()
            .release
            .replace(|character: char| !character.is_ascii_alphanumeric(), "_");
        let staging = self.store_root().join("pack-import").join(staging);
        if staging.exists() {
            std::fs::remove_dir_all(&staging).map_err(|source| pack_io(&staging, source))?;
        }
        std::fs::create_dir_all(&staging).map_err(|source| pack_io(&staging, source))?;
        let file = File::open(pack).map_err(|source| pack_io(pack, source))?;
        let mut archive = tar::Archive::new(file);
        archive
            .unpack(&staging)
            .map_err(|source| pack_io(pack, source))?;
        let root = staging.join(PACK_ROOT);
        let manifest_bytes = std::fs::read(root.join("manifest.json"))
            .map_err(|source| pack_io(&root.join("manifest.json"), source))?;
        let signature = std::fs::read_to_string(root.join("manifest.sig"))
            .map_err(|source| pack_io(&root.join("manifest.sig"), source))?;
        if manifest_bytes != self.manifest().bytes()
            || signature.trim() != self.manifest().signature().trim()
        {
            return Err(ModelManagerError::Manifest(
                crate::manifest::ManifestError::UntrustedSignature,
            ));
        }
        for model in &self.manifest().manifest().models {
            let source = root.join("models").join(&model.id);
            if !source.is_dir() {
                continue;
            }
            for artifact in &model.artifacts {
                verify_artifact(artifact, &source.join(&artifact.path))?;
            }
            run_post_install(model, &source)?;
            verify_group(model, &source)?;
            self.write_receipt(model, &source)?;
            self.activate(model, &source)?;
        }
        std::fs::remove_dir_all(&staging).map_err(|source| pack_io(&staging, source))?;
        Ok(self.statuses())
    }
}

fn append_bytes(
    archive: &mut tar::Builder<File>,
    path: &str,
    bytes: &[u8],
) -> Result<(), ModelManagerError> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive
        .append_data(&mut header, path, Cursor::new(bytes))
        .map_err(|source| pack_io(Path::new(path), source))
}

fn pack_io(path: &Path, source: std::io::Error) -> ModelManagerError {
    ModelManagerError::Io {
        path: PathBuf::from(path),
        source,
    }
}
