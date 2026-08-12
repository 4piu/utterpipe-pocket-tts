use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use atomicwrites::{AllowOverwrite, AtomicFile};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    audio,
    xn_bundle::{
        APRIL_MODEL_ID, VerifiedXnBundle, XnBundleError, verify_bundle as verify_xn_bundle,
    },
    xn_engine::XnVoiceEncoder,
};

const SCHEMA_VERSION: u32 = 1;
static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct Store {
    data_dir: PathBuf,
    cache_dir: PathBuf,
}

pub struct XnModelAssets {
    pub bundle: VerifiedXnBundle,
    _model_lease: File,
}

pub struct XnRuntimeAssets {
    pub bundle: VerifiedXnBundle,
    pub voice_state: PathBuf,
    _model_lease: File,
    _voice_lease: File,
    _voice_state_lease: File,
}

/// Exclusive cross-process lease held for the full lifetime of one mutation.
pub struct MutationGuard {
    _lock: File,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoiceProvenance {
    pub kind: String,
    pub name: String,
    pub source_url: String,
    pub repository: String,
    pub revision: String,
    pub path: String,
    pub license_id: String,
    pub license_url: String,
    pub attribution: String,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("provider storage paths are invalid")]
    InvalidPaths,
    #[error("provider storage is unavailable")]
    Io,
    #[error("provider operational schema is unsupported")]
    Schema,
    #[error("selected model is not installed")]
    ModelMissing,
    #[error("selected voice is not installed")]
    VoiceMissing,
    #[error("installed asset failed integrity validation")]
    Integrity,
    #[error("another provider process is mutating or using this asset")]
    ResourceBusy,
    #[error("voice ID is invalid")]
    InvalidVoiceId,
    #[error("voice import requires explicit consent confirmation")]
    ConsentRequired,
    #[error("voice ID already contains different reference content")]
    VoiceConflict,
    #[error("reference audio is invalid")]
    InvalidAudio,
    #[error("all required model disclosures must be accepted")]
    LicenseRequired,
    #[error("management operation was cancelled")]
    Cancelled,
}

#[derive(Serialize, Deserialize)]
struct SchemaFile {
    schema_version: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct XnInstallation {
    schema_version: u32,
    model_id: String,
    bundle_revision: String,
    accepted_licenses: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct XnVoiceStateManifest {
    schema_version: u32,
    model_id: String,
    bundle_revision: String,
    voice_id: String,
    reference_version: String,
    state_bytes: u64,
    state_sha256: String,
}

#[derive(Serialize, Deserialize)]
struct VoiceMetadata {
    schema_version: u32,
    voice_id: String,
    model_id: String,
    source_sha256: String,
    samples_sha256: String,
    sample_rate_hz: u32,
    sample_count: usize,
    imported_unix_seconds: u64,
    consent_confirmed: bool,
    normalized_wav_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provenance: Option<VoiceProvenance>,
}

impl Store {
    /// Construct a storage view without creating or modifying either root.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::InvalidPaths`] unless both roots are distinct absolute paths.
    pub fn new(data_dir: PathBuf, cache_dir: PathBuf) -> Result<Self, StoreError> {
        if !data_dir.is_absolute()
            || !cache_dir.is_absolute()
            || paths_equivalent(&data_dir, &cache_dir)
        {
            return Err(StoreError::InvalidPaths);
        }
        Ok(Self {
            data_dir,
            cache_dir,
        })
    }

    #[must_use]
    pub fn xn_model_status(&self) -> &'static str {
        match self
            .active_xn_model_dir()
            .and_then(|path| verify_xn_installation(&path).map(|_| ()))
        {
            Ok(()) => "installed",
            Err(StoreError::ModelMissing) => "available",
            Err(_) => "incomplete",
        }
    }

    /// Validate an existing operational schema without creating it.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Schema`] for unreadable or unsupported metadata.
    pub fn validate_local(&self) -> Result<(), StoreError> {
        self.validate_schema_if_present()
    }

    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Return the active immutable token for a logical artifact.
    ///
    /// # Errors
    ///
    /// Returns missing or invalid-selection errors for absent/unknown artifacts.
    pub fn artifact_token(&self, artifact: &str) -> Result<String, StoreError> {
        if artifact == format!("model:{APRIL_MODEL_ID}") {
            read_active(&self.data_dir.join("models").join(APRIL_MODEL_ID))
        } else if let Some(voice) = artifact.strip_prefix("voice:") {
            if !valid_voice_id(voice) {
                return Err(StoreError::InvalidVoiceId);
            }
            read_active(&self.data_dir.join("voices").join(voice))
                .map_err(|_| StoreError::VoiceMissing)
        } else {
            Err(StoreError::InvalidVoiceId)
        }
    }

    /// Return imported voice descriptors without changing storage.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if schema or voice metadata is corrupt.
    pub fn voice_catalog(&self) -> Result<Vec<Value>, StoreError> {
        self.validate_schema_if_present()?;
        let root = self.data_dir.join("voices");
        let Ok(entries) = fs::read_dir(root) else {
            return Ok(Vec::new());
        };
        let mut voices = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|_| StoreError::Io)?;
            if !entry.file_type().map_err(|_| StoreError::Io)?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            if !valid_voice_id(&id) {
                return Err(StoreError::Integrity);
            }
            let version = match read_active(&entry.path()) {
                Ok(version) => version,
                Err(StoreError::ModelMissing) => continue,
                Err(error) => return Err(error),
            };
            let metadata = read_voice_metadata(&entry.path().join("versions").join(version))?;
            let descriptor = if let Some(provenance) = metadata.provenance {
                json!({
                    "id": metadata.voice_id,
                    "name": provenance.name,
                    "status": "installed",
                    "kind": provenance.kind,
                    "languages": ["en"],
                    "license": {
                        "id": provenance.license_id,
                        "url": provenance.license_url,
                        "requires_acceptance": false
                    },
                    "source": {
                        "url": provenance.source_url,
                        "repository": provenance.repository,
                        "revision": provenance.revision,
                        "path": provenance.path,
                        "attribution": provenance.attribution
                    }
                })
            } else {
                json!({
                    "id": metadata.voice_id,
                    "name": metadata.voice_id,
                    "status": "installed",
                    "kind": "imported",
                    "languages": [],
                    "license": {
                        "id": "user-provided-reference",
                        "url": "https://github.com/4piu/utterpipe-pocket-tts#voice-provenance-and-consent",
                        "requires_acceptance": false
                    }
                })
            };
            voices.push(descriptor);
        }
        voices.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        Ok(voices)
    }

    /// Verify and acquire a shared lease on the installed XN model bundle.
    ///
    /// # Errors
    ///
    /// Returns a stable missing, integrity, schema, I/O, or lease error.
    pub fn acquire_xn_model(&self, model_id: &str) -> Result<XnModelAssets, StoreError> {
        self.validate_schema_required()?;
        if model_id != APRIL_MODEL_ID {
            return Err(StoreError::ModelMissing);
        }
        let model_dir = self.active_xn_model_dir()?;
        let bundle = verify_xn_installation(&model_dir)?;
        if model_dir.file_name().and_then(|name| name.to_str()) != Some(bundle.revision.as_str()) {
            return Err(StoreError::Integrity);
        }
        let model_lease = open_shared_lease(&model_dir.join("lease.lock"))?;
        Ok(XnModelAssets {
            bundle,
            _model_lease: model_lease,
        })
    }

    /// Acquire verified model, reference, and prepared voice-state leases for XN.
    ///
    /// # Errors
    ///
    /// Returns missing, integrity, schema, I/O, or concurrent-removal errors.
    pub fn acquire_xn_runtime(
        &self,
        model_id: &str,
        voice_id: &str,
    ) -> Result<XnRuntimeAssets, StoreError> {
        let model = self.acquire_xn_model(model_id)?;
        if !valid_voice_id(voice_id) {
            return Err(StoreError::VoiceMissing);
        }
        let voice_root = self.data_dir.join("voices").join(voice_id);
        let reference_version = read_active(&voice_root).map_err(|error| match error {
            StoreError::ModelMissing => StoreError::VoiceMissing,
            other => other,
        })?;
        let voice_dir = voice_root.join("versions").join(&reference_version);
        verify_voice_dir(&voice_dir, voice_id)?;
        let voice_lease = open_shared_lease(&voice_dir.join("lease.lock"))?;
        let state_root = self
            .data_dir
            .join("voice-states")
            .join(APRIL_MODEL_ID)
            .join(voice_id);
        let state_version = read_active(&state_root).map_err(|_| StoreError::VoiceMissing)?;
        let state_dir = state_root.join("versions").join(&state_version);
        let voice_state =
            verify_xn_voice_state(&state_dir, &model.bundle, voice_id, &reference_version)?;
        if state_version != expected_xn_voice_state_version(&model.bundle, &reference_version) {
            return Err(StoreError::Integrity);
        }
        let voice_state_lease = open_shared_lease(&state_dir.join("lease.lock"))?;
        Ok(XnRuntimeAssets {
            bundle: model.bundle,
            voice_state,
            _model_lease: model._model_lease,
            _voice_lease: voice_lease,
            _voice_state_lease: voice_state_lease,
        })
    }

    /// Install and activate a validated extracted XN model bundle.
    ///
    /// # Errors
    ///
    /// Returns a lock, disclosure, integrity, cancellation, native-load, or
    /// storage error without activating partial contents.
    pub fn install_xn_bundle_from_directory(
        &self,
        source: &Path,
        accepted_licenses: &[String],
    ) -> Result<(), StoreError> {
        let mutation = self.begin_mutation()?;
        self.install_xn_bundle_from_directory_locked(source, accepted_licenses, &mutation, || false)
    }

    /// Install an extracted XN bundle while a caller-owned mutation lease is held.
    ///
    /// # Errors
    ///
    /// Returns a disclosure, integrity, cancellation, native-load, or storage error.
    pub fn install_xn_bundle_from_directory_locked<F>(
        &self,
        source: &Path,
        accepted_licenses: &[String],
        _mutation: &MutationGuard,
        cancelled: F,
    ) -> Result<(), StoreError>
    where
        F: Fn() -> bool,
    {
        check_cancelled(&cancelled)?;
        let source_bundle = verify_xn_bundle(source, &cancelled).map_err(xn_bundle_error)?;
        let accepted: HashSet<_> = accepted_licenses.iter().map(String::as_str).collect();
        if !source_bundle
            .manifest
            .licenses
            .iter()
            .all(|license| accepted.contains(license.id.as_str()))
        {
            return Err(StoreError::LicenseRequired);
        }
        self.initialize_schema()?;
        let root = self.data_dir.join("models").join(APRIL_MODEL_ID);
        let versions = root.join("versions");
        fs::create_dir_all(&versions).map_err(|_| StoreError::Io)?;
        let destination = versions.join(&source_bundle.revision);
        let needs_install = if destination.exists() {
            match verify_xn_installation(&destination) {
                Ok(installed) if installed.revision == source_bundle.revision => false,
                _ => {
                    let lease_path = destination.join("lease.lock");
                    if lease_path.exists() {
                        let lease = OpenOptions::new()
                            .read(true)
                            .write(true)
                            .open(&lease_path)
                            .map_err(|_| StoreError::Integrity)?;
                        lease
                            .try_lock_exclusive()
                            .map_err(|_| StoreError::ResourceBusy)?;
                    }
                    fs::remove_dir_all(&destination).map_err(|_| StoreError::Io)?;
                    sync_dir(&versions)?;
                    true
                }
            }
        } else {
            true
        };
        if needs_install {
            let staging = versions.join(format!(".staging-{}", operation_id()));
            fs::create_dir(&staging).map_err(|_| StoreError::Io)?;
            let result = (|| {
                for name in [
                    "manifest.json",
                    "config.json",
                    "model.gguf",
                    source_bundle.tokenizer_name(),
                ] {
                    check_cancelled(&cancelled)?;
                    copy_file_cancelled(&source.join(name), &staging.join(name), &cancelled)?;
                }
                let staged = verify_xn_bundle(&staging, &cancelled).map_err(xn_bundle_error)?;
                if staged.revision != source_bundle.revision {
                    return Err(StoreError::Integrity);
                }
                check_cancelled(&cancelled)?;
                XnVoiceEncoder::create(&staged.config_path(), &staged.model_path(), 1)
                    .map_err(|_| StoreError::Integrity)?;
                check_cancelled(&cancelled)?;
                write_json_synced(
                    &staging.join("installation.json"),
                    &XnInstallation {
                        schema_version: SCHEMA_VERSION,
                        model_id: APRIL_MODEL_ID.to_owned(),
                        bundle_revision: staged.revision,
                        accepted_licenses: accepted_licenses.to_vec(),
                    },
                )?;
                File::create(staging.join("lease.lock"))
                    .and_then(|file| file.sync_all())
                    .map_err(|_| StoreError::Io)?;
                sync_dir(&staging)?;
                check_cancelled(&cancelled)?;
                fs::rename(&staging, &destination).map_err(|_| StoreError::Io)?;
                sync_dir(&versions)
            })();
            if result.is_err() {
                let _ = fs::remove_dir_all(&staging);
            }
            result?;
        }
        check_cancelled(&cancelled)?;
        publish_active(&root, &source_bundle.revision)
    }

    /// Prepare and activate an XN voice state from an installed reference.
    ///
    /// # Errors
    ///
    /// Returns a lock, missing-asset, integrity, cancellation, inference, or
    /// storage error without publishing a partial state.
    pub fn prepare_xn_voice(
        &self,
        model_id: &str,
        voice_id: &str,
        num_threads: u32,
    ) -> Result<(), StoreError> {
        let mutation = self.begin_mutation()?;
        self.prepare_xn_voice_locked(model_id, voice_id, num_threads, &mutation, || false)
    }

    /// Prepare a voice while a caller-owned mutation lease is held.
    ///
    /// # Errors
    ///
    /// Returns a missing-asset, integrity, cancellation, inference, or storage error.
    pub fn prepare_xn_voice_locked<F>(
        &self,
        model_id: &str,
        voice_id: &str,
        num_threads: u32,
        _mutation: &MutationGuard,
        cancelled: F,
    ) -> Result<(), StoreError>
    where
        F: Fn() -> bool,
    {
        check_cancelled(&cancelled)?;
        if model_id != APRIL_MODEL_ID || !valid_voice_id(voice_id) {
            return Err(StoreError::VoiceMissing);
        }
        let model = self.acquire_xn_model(model_id)?;
        let voice_root = self.data_dir.join("voices").join(voice_id);
        let reference_version = read_active(&voice_root).map_err(|_| StoreError::VoiceMissing)?;
        let voice_dir = voice_root.join("versions").join(&reference_version);
        let metadata = read_voice_metadata(&voice_dir)?;
        verify_voice_dir(&voice_dir, voice_id)?;
        let _voice_lease = open_shared_lease(&voice_dir.join("lease.lock"))?;
        let reference = audio::read_reference(&voice_dir.join("reference.wav"), 30.0)
            .map_err(|_| StoreError::Integrity)?;
        if metadata.samples_sha256 != reference_version
            || metadata.samples_sha256 != reference.samples_sha256
        {
            return Err(StoreError::Integrity);
        }
        let state_version = expected_xn_voice_state_version(&model.bundle, &reference_version);
        let root = self
            .data_dir
            .join("voice-states")
            .join(APRIL_MODEL_ID)
            .join(voice_id);
        let versions = root.join("versions");
        fs::create_dir_all(&versions).map_err(|_| StoreError::Io)?;
        let destination = versions.join(&state_version);
        if destination.exists() {
            verify_xn_voice_state(&destination, &model.bundle, voice_id, &reference_version)?;
        } else {
            let staging = versions.join(format!(".staging-{}", operation_id()));
            fs::create_dir(&staging).map_err(|_| StoreError::Io)?;
            let result = (|| {
                let state_path = staging.join("voice.safetensors");
                let encoder = XnVoiceEncoder::create(
                    &model.bundle.config_path(),
                    &model.bundle.model_path(),
                    num_threads,
                )
                .map_err(xn_engine_error)?;
                encoder
                    .prepare_voice(&reference, &state_path, &cancelled)
                    .map_err(xn_engine_error)?;
                check_cancelled(&cancelled)?;
                let state_bytes = fs::metadata(&state_path).map_err(|_| StoreError::Io)?.len();
                let state_sha256 = hash_file_cancelled(&state_path, &cancelled)?;
                write_json_synced(
                    &staging.join("manifest.json"),
                    &XnVoiceStateManifest {
                        schema_version: SCHEMA_VERSION,
                        model_id: APRIL_MODEL_ID.to_owned(),
                        bundle_revision: model.bundle.revision.clone(),
                        voice_id: voice_id.to_owned(),
                        reference_version: reference_version.clone(),
                        state_bytes,
                        state_sha256,
                    },
                )?;
                File::create(staging.join("lease.lock"))
                    .and_then(|file| file.sync_all())
                    .map_err(|_| StoreError::Io)?;
                sync_dir(&staging)?;
                check_cancelled(&cancelled)?;
                fs::rename(&staging, &destination).map_err(|_| StoreError::Io)?;
                sync_dir(&versions)
            })();
            if result.is_err() {
                let _ = fs::remove_dir_all(&staging);
            }
            result?;
        }
        check_cancelled(&cancelled)?;
        publish_active(&root, &state_version)
    }

    /// Import and atomically activate one normalized reference voice.
    ///
    /// # Errors
    ///
    /// Returns a consent, identifier, audio, conflict, locking, schema, or I/O error.
    pub fn import_voice(
        &self,
        source: &Path,
        voice_id: &str,
        consent_confirmed: bool,
        maximum_seconds: f64,
    ) -> Result<(), StoreError> {
        if !consent_confirmed {
            return Err(StoreError::ConsentRequired);
        }
        if !source.is_absolute() || !valid_voice_id(voice_id) {
            return Err(StoreError::InvalidVoiceId);
        }
        let mutation = self.begin_mutation()?;
        self.import_voice_inner(source, voice_id, maximum_seconds, &mutation, None, || false)
    }

    fn import_voice_inner<F>(
        &self,
        source: &Path,
        voice_id: &str,
        maximum_seconds: f64,
        _mutation: &MutationGuard,
        provenance: Option<VoiceProvenance>,
        cancelled: F,
    ) -> Result<(), StoreError>
    where
        F: Fn() -> bool,
    {
        if !source.is_absolute() || !valid_voice_id(voice_id) {
            return Err(StoreError::InvalidVoiceId);
        }
        check_cancelled(&cancelled)?;
        let reference = audio::read_reference_cancelled(source, maximum_seconds, &cancelled)
            .map_err(|error| match error {
                audio::AudioError::Cancelled => StoreError::Cancelled,
                _ => StoreError::InvalidAudio,
            })?;
        check_cancelled(&cancelled)?;
        self.initialize_schema()?;
        let root = self.data_dir.join("voices").join(voice_id);
        if let Ok(active) = read_active(&root) {
            if active == reference.samples_sha256 {
                verify_voice_dir(&root.join("versions").join(active), voice_id)?;
                return Ok(());
            }
            return Err(StoreError::VoiceConflict);
        }
        let versions = root.join("versions");
        check_cancelled(&cancelled)?;
        fs::create_dir_all(&versions).map_err(|_| StoreError::Io)?;
        let version = reference.samples_sha256.clone();
        let destination = versions.join(&version);
        if !destination.exists() {
            let staging = versions.join(format!(".staging-{}", operation_id()));
            fs::create_dir(&staging).map_err(|_| StoreError::Io)?;
            let result = (|| {
                check_cancelled(&cancelled)?;
                audio::write_pcm16_wav(
                    &staging.join("reference.wav"),
                    reference.sample_rate,
                    &reference.samples,
                )
                .map_err(|_| StoreError::Io)?;
                check_cancelled(&cancelled)?;
                let normalized_wav_sha256 = hash_file(&staging.join("reference.wav"))?;
                let metadata = VoiceMetadata {
                    schema_version: SCHEMA_VERSION,
                    voice_id: voice_id.to_owned(),
                    model_id: APRIL_MODEL_ID.to_owned(),
                    source_sha256: reference.source_sha256.clone(),
                    samples_sha256: reference.samples_sha256.clone(),
                    sample_rate_hz: reference.sample_rate,
                    sample_count: reference.samples.len(),
                    imported_unix_seconds: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|_| StoreError::Io)?
                        .as_secs(),
                    consent_confirmed: true,
                    normalized_wav_sha256,
                    provenance,
                };
                write_json_synced(&staging.join("metadata.json"), &metadata)?;
                File::create(staging.join("lease.lock"))
                    .and_then(|file| file.sync_all())
                    .map_err(|_| StoreError::Io)?;
                sync_dir(&staging)?;
                check_cancelled(&cancelled)?;
                fs::rename(&staging, &destination).map_err(|_| StoreError::Io)?;
                sync_dir(&versions)
            })();
            if result.is_err() {
                let _ = fs::remove_dir_all(&staging);
            }
            result?;
        } else {
            verify_voice_dir(&destination, voice_id)?;
        }
        check_cancelled(&cancelled)?;
        publish_active(&root, &version)
    }

    /// Import a voice while a caller-owned mutation lease is held.
    ///
    /// # Errors
    ///
    /// Returns a consent, identifier, audio, conflict, schema, or I/O error.
    pub fn import_voice_locked<F>(
        &self,
        source: &Path,
        voice_id: &str,
        consent_confirmed: bool,
        maximum_seconds: f64,
        mutation: &MutationGuard,
        cancelled: F,
    ) -> Result<(), StoreError>
    where
        F: Fn() -> bool,
    {
        if !consent_confirmed {
            return Err(StoreError::ConsentRequired);
        }
        if !source.is_absolute() || !valid_voice_id(voice_id) {
            return Err(StoreError::InvalidVoiceId);
        }
        self.import_voice_inner(source, voice_id, maximum_seconds, mutation, None, cancelled)
    }

    /// Import a voice with verified curated-source provenance while a
    /// caller-owned mutation lease is held.
    ///
    /// # Errors
    ///
    /// Returns a consent, identifier, audio, conflict, schema, cancellation,
    /// locking, or I/O error.
    #[allow(clippy::too_many_arguments)]
    pub fn import_curated_voice_locked<F>(
        &self,
        source: &Path,
        voice_id: &str,
        consent_confirmed: bool,
        maximum_seconds: f64,
        mutation: &MutationGuard,
        provenance: VoiceProvenance,
        cancelled: F,
    ) -> Result<(), StoreError>
    where
        F: Fn() -> bool,
    {
        if !consent_confirmed {
            return Err(StoreError::ConsentRequired);
        }
        if !source.is_absolute() || !valid_voice_id(voice_id) {
            return Err(StoreError::InvalidVoiceId);
        }
        self.import_voice_inner(
            source,
            voice_id,
            maximum_seconds,
            mutation,
            Some(provenance),
            cancelled,
        )
    }

    /// Remove exact model/voice artifacts while refusing active leases.
    ///
    /// # Errors
    ///
    /// Returns missing, unsafe-ID, lock, lease, schema, or I/O errors. No requested
    /// artifact is removed unless every requested lease can first be held exclusively.
    pub fn remove_artifacts(&self, artifacts: &[String]) -> Result<Vec<String>, StoreError> {
        let mutation = self.begin_mutation()?;
        self.remove_artifacts_locked(artifacts, &mutation, || false)
    }

    /// Remove artifacts while a caller-owned mutation lease is held.
    ///
    /// # Errors
    ///
    /// Returns missing, selection, lease, schema, cancellation, or I/O errors.
    pub fn remove_artifacts_locked<F>(
        &self,
        artifacts: &[String],
        _mutation: &MutationGuard,
        cancelled: F,
    ) -> Result<Vec<String>, StoreError>
    where
        F: Fn() -> bool,
    {
        if artifacts.is_empty() || artifacts.iter().collect::<HashSet<_>>().len() != artifacts.len()
        {
            return Err(StoreError::InvalidVoiceId);
        }
        check_cancelled(&cancelled)?;
        self.validate_schema_required()?;
        let mut leases = Vec::new();
        let mut roots = Vec::new();
        let mut derived_roots = Vec::new();
        for artifact in artifacts {
            check_cancelled(&cancelled)?;
            let (root, version) = if artifact == &format!("model:{APRIL_MODEL_ID}") {
                let root = self.data_dir.join("models").join(APRIL_MODEL_ID);
                let version = read_active(&root)?;
                (root, version)
            } else if let Some(voice) = artifact.strip_prefix("voice:") {
                if !valid_voice_id(voice) {
                    return Err(StoreError::InvalidVoiceId);
                }
                let root = self.data_dir.join("voices").join(voice);
                let version = read_active(&root).map_err(|_| StoreError::VoiceMissing)?;
                derived_roots.push(
                    self.data_dir
                        .join("voice-states")
                        .join(APRIL_MODEL_ID)
                        .join(voice),
                );
                (root, version)
            } else {
                return Err(StoreError::InvalidVoiceId);
            };
            let lease = OpenOptions::new()
                .read(true)
                .write(true)
                .open(root.join("versions").join(&version).join("lease.lock"))
                .map_err(|_| StoreError::Integrity)?;
            lease
                .try_lock_exclusive()
                .map_err(|_| StoreError::ResourceBusy)?;
            leases.push(lease);
            roots.push(root);
        }
        let transaction = operation_id();
        let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
        for root in &roots {
            if cancelled() {
                rollback_active_pointers(&staged)?;
                return Err(StoreError::Cancelled);
            }
            let active = root.join("active");
            let tombstone = root.join(format!(".removing-{transaction}"));
            if fs::rename(&active, &tombstone).is_err() {
                rollback_active_pointers(&staged)?;
                return Err(StoreError::Io);
            }
            staged.push((active, tombstone));
        }
        drop(leases);
        for root in roots {
            // Active pointers are already removed as one committed logical operation.
            // Residual immutable versions are safe garbage and may be cleaned later.
            let _ = fs::remove_dir_all(root);
        }
        for root in derived_roots {
            let _ = fs::remove_dir_all(root);
        }
        Ok(artifacts.to_vec())
    }

    fn active_xn_model_dir(&self) -> Result<PathBuf, StoreError> {
        let root = self.data_dir.join("models").join(APRIL_MODEL_ID);
        let version = read_active(&root)?;
        Ok(root.join("versions").join(version))
    }

    fn validate_schema_if_present(&self) -> Result<(), StoreError> {
        let path = self.data_dir.join("schema.json");
        if !path.exists() {
            return Ok(());
        }
        self.validate_schema_required()
    }

    fn validate_schema_required(&self) -> Result<(), StoreError> {
        let bytes = fs::read(self.data_dir.join("schema.json")).map_err(|_| StoreError::Schema)?;
        let schema: SchemaFile = serde_json::from_slice(&bytes).map_err(|_| StoreError::Schema)?;
        if schema.schema_version != SCHEMA_VERSION {
            return Err(StoreError::Schema);
        }
        Ok(())
    }

    fn initialize_schema(&self) -> Result<(), StoreError> {
        fs::create_dir_all(&self.data_dir).map_err(|_| StoreError::Io)?;
        fs::create_dir_all(&self.cache_dir).map_err(|_| StoreError::Io)?;
        let path = self.data_dir.join("schema.json");
        if path.exists() {
            return self.validate_schema_required();
        }
        write_json_synced(
            &path,
            &SchemaFile {
                schema_version: SCHEMA_VERSION,
            },
        )
    }

    /// Acquire the provider-wide, nonblocking cross-process mutation lease.
    ///
    /// # Errors
    ///
    /// Returns `resource_busy` when another process owns the lease.
    pub fn begin_mutation(&self) -> Result<MutationGuard, StoreError> {
        fs::create_dir_all(&self.data_dir).map_err(|_| StoreError::Io)?;
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(self.data_dir.join("mutation.lock"))
            .map_err(|_| StoreError::Io)?;
        lock.try_lock_exclusive()
            .map_err(|_| StoreError::ResourceBusy)?;
        Ok(MutationGuard { _lock: lock })
    }
}

fn verify_xn_installation(path: &Path) -> Result<VerifiedXnBundle, StoreError> {
    if !path.is_dir() {
        return Err(StoreError::ModelMissing);
    }
    let bundle = verify_xn_bundle(path, || false).map_err(xn_bundle_error)?;
    let bytes = fs::read(path.join("installation.json")).map_err(|_| StoreError::Integrity)?;
    let installation: XnInstallation =
        serde_json::from_slice(&bytes).map_err(|_| StoreError::Integrity)?;
    let accepted: HashSet<_> = installation
        .accepted_licenses
        .iter()
        .map(String::as_str)
        .collect();
    if installation.schema_version != SCHEMA_VERSION
        || installation.model_id != APRIL_MODEL_ID
        || installation.bundle_revision != bundle.revision
        || !bundle
            .manifest
            .licenses
            .iter()
            .all(|license| accepted.contains(license.id.as_str()))
    {
        return Err(StoreError::Integrity);
    }
    Ok(bundle)
}

fn expected_xn_voice_state_version(bundle: &VerifiedXnBundle, reference_version: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(bundle.revision.as_bytes());
    digest.update(b":");
    digest.update(reference_version.as_bytes());
    format!("{:x}", digest.finalize())
}

fn verify_xn_voice_state(
    path: &Path,
    bundle: &VerifiedXnBundle,
    voice_id: &str,
    reference_version: &str,
) -> Result<PathBuf, StoreError> {
    if !path.is_dir() {
        return Err(StoreError::VoiceMissing);
    }
    let bytes = fs::read(path.join("manifest.json")).map_err(|_| StoreError::Integrity)?;
    let manifest: XnVoiceStateManifest =
        serde_json::from_slice(&bytes).map_err(|_| StoreError::Integrity)?;
    let state_path = path.join("voice.safetensors");
    if manifest.schema_version != SCHEMA_VERSION
        || manifest.model_id != APRIL_MODEL_ID
        || manifest.bundle_revision != bundle.revision
        || manifest.voice_id != voice_id
        || manifest.reference_version != reference_version
        || fs::metadata(&state_path)
            .map_err(|_| StoreError::Integrity)?
            .len()
            != manifest.state_bytes
        || hash_file(&state_path)? != manifest.state_sha256
    {
        return Err(StoreError::Integrity);
    }
    Ok(state_path)
}

fn xn_bundle_error(error: XnBundleError) -> StoreError {
    match error {
        XnBundleError::InvalidPath | XnBundleError::InvalidManifest | XnBundleError::Integrity => {
            StoreError::Integrity
        }
        XnBundleError::Cancelled => StoreError::Cancelled,
        XnBundleError::Io => StoreError::Io,
    }
}

fn xn_engine_error(error: crate::engine::EngineError) -> StoreError {
    match error {
        crate::engine::EngineError::Cancelled => StoreError::Cancelled,
        _ => StoreError::Integrity,
    }
}

fn read_voice_metadata(path: &Path) -> Result<VoiceMetadata, StoreError> {
    let bytes = fs::read(path.join("metadata.json")).map_err(|_| StoreError::Integrity)?;
    let metadata: VoiceMetadata =
        serde_json::from_slice(&bytes).map_err(|_| StoreError::Integrity)?;
    if metadata.schema_version != SCHEMA_VERSION || !valid_voice_provenance(&metadata) {
        return Err(StoreError::Integrity);
    }
    Ok(metadata)
}

fn valid_voice_provenance(metadata: &VoiceMetadata) -> bool {
    let Some(provenance) = metadata.provenance.as_ref() else {
        return true;
    };
    crate::voice::CURATED_VOICES.iter().any(|voice| {
        provenance.kind == "curated"
            && provenance.name == voice.name
            && provenance.source_url == crate::voice::curated_download_url(voice).as_str()
            && provenance.repository == voice.repository
            && provenance.revision == voice.revision
            && provenance.path == voice.path
            && provenance.license_id == voice.license_id
            && provenance.license_url == voice.license_url
            && provenance.attribution == voice.attribution
            && metadata.source_sha256 == voice.sha256
    })
}

fn verify_voice_dir(path: &Path, expected_voice_id: &str) -> Result<(), StoreError> {
    let metadata = read_voice_metadata(path)?;
    if metadata.voice_id != expected_voice_id
        || metadata.model_id != APRIL_MODEL_ID
        || !metadata.consent_confirmed
        || hash_file(&path.join("reference.wav"))? != metadata.normalized_wav_sha256
    {
        return Err(StoreError::Integrity);
    }
    let reference = audio::read_reference(&path.join("reference.wav"), 30.0)
        .map_err(|_| StoreError::Integrity)?;
    if reference.samples_sha256 != metadata.samples_sha256
        || reference.sample_rate != metadata.sample_rate_hz
        || reference.samples.len() != metadata.sample_count
    {
        return Err(StoreError::Integrity);
    }
    Ok(())
}

fn read_active(root: &Path) -> Result<String, StoreError> {
    let token = fs::read_to_string(root.join("active")).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            StoreError::ModelMissing
        } else {
            StoreError::Io
        }
    })?;
    let token = token.trim();
    if token.is_empty()
        || token.len() > 128
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Err(StoreError::Integrity);
    }
    Ok(token.to_owned())
}

fn publish_active(root: &Path, token: &str) -> Result<(), StoreError> {
    fs::create_dir_all(root).map_err(|_| StoreError::Io)?;
    AtomicFile::new(root.join("active"), AllowOverwrite)
        .write::<_, std::io::Error, _>(|file| writeln!(file, "{token}"))
        .map_err(|_| StoreError::Io)?;
    sync_dir(root)
}

fn open_shared_lease(path: &Path) -> Result<File, StoreError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|_| StoreError::Integrity)?;
    FileExt::try_lock_shared(&file).map_err(|_| StoreError::ResourceBusy)?;
    Ok(file)
}

fn write_json_synced(path: &Path, value: &impl Serialize) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| StoreError::Io)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| StoreError::Io)?;
    file.write_all(&bytes).map_err(|_| StoreError::Io)?;
    file.write_all(b"\n").map_err(|_| StoreError::Io)?;
    file.sync_all().map_err(|_| StoreError::Io)
}

fn hash_file(path: &Path) -> Result<String, StoreError> {
    hash_file_cancelled(path, &|| false)
}

fn hash_file_cancelled<F>(path: &Path, cancelled: &F) -> Result<String, StoreError>
where
    F: Fn() -> bool,
{
    let metadata = fs::symlink_metadata(path).map_err(|_| StoreError::Io)?;
    if !metadata.file_type().is_file() {
        return Err(StoreError::Integrity);
    }
    let mut file = File::open(path).map_err(|_| StoreError::Io)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1_024];
    loop {
        check_cancelled(cancelled)?;
        let count = file.read(&mut buffer).map_err(|_| StoreError::Io)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn copy_file_cancelled<F>(
    source: &Path,
    destination: &Path,
    cancelled: &F,
) -> Result<(), StoreError>
where
    F: Fn() -> bool,
{
    let mut source = File::open(source).map_err(|_| StoreError::Io)?;
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|_| StoreError::Io)?;
    copy_reader_cancelled(&mut source, &mut destination, cancelled)?;
    destination.sync_all().map_err(|_| StoreError::Io)
}

fn copy_reader_cancelled<R, W, F>(
    reader: &mut R,
    writer: &mut W,
    cancelled: &F,
) -> Result<(), StoreError>
where
    R: Read,
    W: Write,
    F: Fn() -> bool,
{
    let mut buffer = [0_u8; 64 * 1_024];
    loop {
        check_cancelled(cancelled)?;
        let count = reader.read(&mut buffer).map_err(|_| StoreError::Io)?;
        if count == 0 {
            return Ok(());
        }
        writer
            .write_all(&buffer[..count])
            .map_err(|_| StoreError::Io)?;
    }
}

fn check_cancelled<F>(cancelled: &F) -> Result<(), StoreError>
where
    F: Fn() -> bool,
{
    if cancelled() {
        Err(StoreError::Cancelled)
    } else {
        Ok(())
    }
}

fn rollback_active_pointers(staged: &[(PathBuf, PathBuf)]) -> Result<(), StoreError> {
    for (active, tombstone) in staged.iter().rev() {
        if fs::rename(tombstone, active).is_err() {
            return Err(StoreError::Io);
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn sync_dir(path: &Path) -> Result<(), StoreError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| StoreError::Io)
}

#[cfg(windows)]
fn sync_dir(path: &Path) -> Result<(), StoreError> {
    // Rust's safe std API cannot open a directory handle for flushing on
    // Windows. Validate that the target still is a directory; the preceding
    // file flushes and atomic rename remain the available durability boundary.
    if path.is_dir() {
        Ok(())
    } else {
        Err(StoreError::Io)
    }
}

fn operation_id() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn valid_voice_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if !(1..=64).contains(&bytes.len())
        || !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit()
    {
        return false;
    }
    if bytes.len() > 1
        && !bytes[bytes.len() - 1].is_ascii_lowercase()
        && !bytes[bytes.len() - 1].is_ascii_digit()
    {
        return false;
    }
    bytes.iter().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    })
}

fn paths_equivalent(first: &Path, second: &Path) -> bool {
    if normalize_path(first) == normalize_path(second) {
        return true;
    }
    match (fs::canonicalize(first), fs::canonicalize(second)) {
        (Ok(first), Ok(second)) => first == second,
        _ => false,
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn voice_ids_are_strict() {
        for valid in ["a", "my-voice", "voice_1", "v.2"] {
            assert!(valid_voice_id(valid));
        }
        for invalid in ["", "-voice", "voice-", "Voice", "a/b"] {
            assert!(!valid_voice_id(invalid));
        }
    }

    fn test_store() -> (TempDir, Store) {
        let temp = TempDir::new().unwrap();
        let store = Store::new(temp.path().join("data"), temp.path().join("cache")).unwrap();
        (temp, store)
    }

    fn reference_with_extra_chunk(path: &Path, value: i16) {
        let base = path.with_extension("base.wav");
        audio::write_pcm16_wav(&base, 16_000, &vec![value; 16_000]).unwrap();
        let bytes = fs::read(&base).unwrap();
        let mut output = Vec::with_capacity(bytes.len() + 12);
        output.extend_from_slice(&bytes[..36]);
        output.extend_from_slice(b"JUNK");
        output.extend_from_slice(&3_u32.to_le_bytes());
        output.extend_from_slice(b"abc\0");
        output.extend_from_slice(&bytes[36..]);
        let riff_size = u32::try_from(output.len() - 8).unwrap();
        output[4..8].copy_from_slice(&riff_size.to_le_bytes());
        fs::write(path, output).unwrap();
    }

    #[test]
    fn lexical_root_aliases_are_rejected() {
        let temp = TempDir::new().unwrap();
        let first = temp.path().join("data");
        let alias = first.join("child").join("..");
        assert!(matches!(
            Store::new(first, alias),
            Err(StoreError::InvalidPaths)
        ));
    }

    #[test]
    fn normalized_voice_tracks_source_and_artifact_hashes_separately() {
        let (temp, store) = test_store();
        let source = temp.path().join("source.wav");
        reference_with_extra_chunk(&source, 100);
        store
            .import_voice(&source, "test-voice", true, 30.0)
            .unwrap();
        let root = temp.path().join("data/voices/test-voice");
        let version = read_active(&root).unwrap();
        let voice_dir = root.join("versions").join(version);
        let metadata = read_voice_metadata(&voice_dir).unwrap();
        assert_ne!(metadata.source_sha256, metadata.normalized_wav_sha256);
        assert_eq!(
            hash_file(&voice_dir.join("reference.wav")).unwrap(),
            metadata.normalized_wav_sha256
        );
        let normalized = audio::read_reference(&voice_dir.join("reference.wav"), 30.0).unwrap();
        assert_eq!(normalized.samples_sha256, metadata.samples_sha256);
    }

    #[test]
    fn curated_voice_catalog_preserves_verified_provenance() {
        let (temp, store) = test_store();
        let source = temp.path().join("curated.wav");
        reference_with_extra_chunk(&source, 100);
        let curated = &crate::voice::CURATED_VOICES[0];
        let mutation = store.begin_mutation().unwrap();
        store
            .import_curated_voice_locked(
                &source,
                "curated",
                true,
                30.0,
                &mutation,
                VoiceProvenance {
                    kind: "curated".to_owned(),
                    name: curated.name.to_owned(),
                    source_url: crate::voice::curated_download_url(curated).to_string(),
                    repository: curated.repository.to_owned(),
                    revision: curated.revision.to_owned(),
                    path: curated.path.to_owned(),
                    license_id: curated.license_id.to_owned(),
                    license_url: curated.license_url.to_owned(),
                    attribution: curated.attribution.to_owned(),
                },
                || false,
            )
            .unwrap();
        drop(mutation);

        // The real curated downloader has already verified this source hash.
        // This unit fixture uses generated audio, so align its stored source
        // identity with the manifest before exercising catalog validation.
        let root = temp.path().join("data/voices/curated");
        let metadata_path = root
            .join("versions")
            .join(read_active(&root).unwrap())
            .join("metadata.json");
        let mut metadata: VoiceMetadata =
            serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
        metadata.source_sha256 = curated.sha256.to_owned();
        let mut metadata_bytes = serde_json::to_vec_pretty(&metadata).unwrap();
        metadata_bytes.push(b'\n');
        fs::write(&metadata_path, metadata_bytes).unwrap();

        let voice = store.voice_catalog().unwrap().remove(0);
        assert_eq!(voice["kind"], "curated");
        assert_eq!(voice["name"], curated.name);
        assert_eq!(voice["license"]["id"], "cc0-1.0");
        assert_eq!(voice["source"]["attribution"], curated.attribution);
    }

    #[test]
    fn identical_decoded_voice_is_idempotent_across_wave_containers() {
        let (temp, store) = test_store();
        let first = temp.path().join("first.wav");
        let second = temp.path().join("second.wav");
        reference_with_extra_chunk(&first, 123);
        audio::write_pcm16_wav(&second, 16_000, &vec![123; 16_000]).unwrap();
        store.import_voice(&first, "same", true, 30.0).unwrap();
        store.import_voice(&second, "same", true, 30.0).unwrap();

        let root = temp.path().join("data/voices/same");
        let metadata =
            read_voice_metadata(&root.join("versions").join(read_active(&root).unwrap())).unwrap();
        assert_eq!(metadata.source_sha256, hash_file(&first).unwrap());
        assert_ne!(metadata.source_sha256, hash_file(&second).unwrap());
    }

    #[test]
    fn mutation_contention_precedes_voice_decoding() {
        let (temp, store) = test_store();
        let competing =
            Store::new(temp.path().join("data"), temp.path().join("other-cache")).unwrap();
        let _guard = store.begin_mutation().unwrap();
        assert!(matches!(
            competing.import_voice(&temp.path().join("missing.wav"), "contended", true, 30.0),
            Err(StoreError::ResourceBusy)
        ));
    }

    #[test]
    fn cancelled_transaction_rolls_back_all_active_pointers() {
        use std::cell::Cell;

        let (temp, store) = test_store();
        let first = temp.path().join("first.wav");
        let second = temp.path().join("second.wav");
        reference_with_extra_chunk(&first, 100);
        reference_with_extra_chunk(&second, 200);
        store.import_voice(&first, "first", true, 30.0).unwrap();
        store.import_voice(&second, "second", true, 30.0).unwrap();
        let guard = store.begin_mutation().unwrap();
        let checks = Cell::new(0_u32);
        let result = store.remove_artifacts_locked(
            &["voice:first".to_owned(), "voice:second".to_owned()],
            &guard,
            || {
                checks.set(checks.get() + 1);
                checks.get() >= 5
            },
        );
        assert!(matches!(result, Err(StoreError::Cancelled)));
        assert!(store.artifact_token("voice:first").is_ok());
        assert!(store.artifact_token("voice:second").is_ok());
    }

    #[test]
    fn leased_voice_refuses_removal() {
        let (temp, store) = test_store();
        let source = temp.path().join("source.wav");
        reference_with_extra_chunk(&source, 100);
        store.import_voice(&source, "leased", true, 30.0).unwrap();
        let root = temp.path().join("data/voices/leased");
        let version = read_active(&root).unwrap();
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("versions").join(version).join("lease.lock"))
            .unwrap();
        FileExt::try_lock_shared(&lease).unwrap();
        assert!(matches!(
            store.remove_artifacts(&["voice:leased".to_owned()]),
            Err(StoreError::ResourceBusy)
        ));
        drop(lease);
        assert!(store.remove_artifacts(&["voice:leased".to_owned()]).is_ok());
    }

    #[test]
    fn multi_voice_removal_commits_all_logical_targets() {
        let (temp, store) = test_store();
        let first = temp.path().join("first.wav");
        let second = temp.path().join("second.wav");
        reference_with_extra_chunk(&first, 100);
        reference_with_extra_chunk(&second, 200);
        store.import_voice(&first, "first", true, 30.0).unwrap();
        store.import_voice(&second, "second", true, 30.0).unwrap();
        store
            .remove_artifacts(&["voice:first".to_owned(), "voice:second".to_owned()])
            .unwrap();
        assert!(matches!(
            store.artifact_token("voice:first"),
            Err(StoreError::VoiceMissing)
        ));
        assert!(matches!(
            store.artifact_token("voice:second"),
            Err(StoreError::VoiceMissing)
        ));
    }
}
