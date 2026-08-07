use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use atomicwrites::{AllowOverwrite, AtomicFile};
use bzip2::read::BzDecoder;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tar::Archive;
use thiserror::Error;

use crate::{
    audio::{self, ReferenceAudio},
    model::{ARCHIVE_SHA256, MODEL_ID, REQUIRED_FILES, VERSION_TOKEN},
};

const SCHEMA_VERSION: u32 = 1;
const MAX_ARCHIVE_ENTRIES: usize = 128;
const MAX_EXTRACTED_BYTES: u64 = 300 * 1_024 * 1_024;
static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct Store {
    data_dir: PathBuf,
    cache_dir: PathBuf,
}

pub struct RuntimeAssets {
    pub model_dir: PathBuf,
    pub reference: ReferenceAudio,
    _model_lease: File,
    _voice_lease: File,
}

/// Exclusive cross-process lease held for the full lifetime of one mutation.
pub struct MutationGuard {
    _lock: File,
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
    #[error("model archive is unsafe or incompatible")]
    UnsafeArchive,
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
struct ModelManifest {
    schema_version: u32,
    model_id: String,
    archive_sha256: String,
    accepted_licenses: Vec<String>,
    files: HashMap<String, String>,
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
    pub fn model_status(&self) -> &'static str {
        match self
            .active_model_dir()
            .and_then(|path| verify_model_dir(&path))
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
        if artifact == format!("model:{MODEL_ID}") {
            read_active(&self.data_dir.join("models").join(MODEL_ID))
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
            voices.push(json!({
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
            }));
        }
        voices.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        Ok(voices)
    }

    /// Verify and acquire shared model and voice leases for a runtime.
    ///
    /// # Errors
    ///
    /// Returns a stable missing, integrity, schema, I/O, or concurrent-removal error.
    pub fn acquire_runtime(
        &self,
        model_id: &str,
        voice_id: &str,
    ) -> Result<RuntimeAssets, StoreError> {
        self.validate_schema_required()?;
        if model_id != MODEL_ID {
            return Err(StoreError::ModelMissing);
        }
        let model_dir = self.active_model_dir()?;
        verify_model_dir(&model_dir)?;
        let model_lease = open_shared_lease(&model_dir.join("lease.lock"))?;

        if !valid_voice_id(voice_id) {
            return Err(StoreError::VoiceMissing);
        }
        let voice_root = self.data_dir.join("voices").join(voice_id);
        let voice_version = read_active(&voice_root).map_err(|error| match error {
            StoreError::ModelMissing => StoreError::VoiceMissing,
            other => other,
        })?;
        let voice_dir = voice_root.join("versions").join(voice_version);
        let metadata = read_voice_metadata(&voice_dir)?;
        if metadata.voice_id != voice_id
            || metadata.model_id != MODEL_ID
            || !metadata.consent_confirmed
        {
            return Err(StoreError::Integrity);
        }
        let voice_lease = open_shared_lease(&voice_dir.join("lease.lock"))?;
        let reference = audio::read_reference(&voice_dir.join("reference.wav"), 30.0)
            .map_err(|_| StoreError::Integrity)?;
        if hash_file(&voice_dir.join("reference.wav"))? != metadata.normalized_wav_sha256
            || reference.samples_sha256 != metadata.samples_sha256
            || reference.sample_rate != metadata.sample_rate_hz
            || reference.samples.len() != metadata.sample_count
        {
            return Err(StoreError::Integrity);
        }
        Ok(RuntimeAssets {
            model_dir,
            reference,
            _model_lease: model_lease,
            _voice_lease: voice_lease,
        })
    }

    /// Install and activate the pinned model from an already-downloaded archive.
    ///
    /// # Errors
    ///
    /// Returns a lock, I/O, hash, archive-policy, or schema error without activating
    /// partial contents.
    pub fn install_model_from_archive(
        &self,
        archive_path: &Path,
        accepted_licenses: &[String],
    ) -> Result<(), StoreError> {
        let accepted: HashSet<_> = accepted_licenses.iter().map(String::as_str).collect();
        if !crate::model::LICENSE_IDS
            .iter()
            .all(|license| accepted.contains(license))
        {
            return Err(StoreError::LicenseRequired);
        }
        let mutation = self.begin_mutation()?;
        self.install_model_from_archive_locked(archive_path, accepted_licenses, &mutation, || false)
    }

    /// Install an archive while a caller-owned mutation lease is held.
    ///
    /// # Errors
    ///
    /// Returns an I/O, integrity, archive-policy, or schema error.
    pub fn install_model_from_archive_locked<F>(
        &self,
        archive_path: &Path,
        accepted_licenses: &[String],
        _mutation: &MutationGuard,
        cancelled: F,
    ) -> Result<(), StoreError>
    where
        F: Fn() -> bool,
    {
        let accepted: HashSet<_> = accepted_licenses.iter().map(String::as_str).collect();
        if !crate::model::LICENSE_IDS
            .iter()
            .all(|license| accepted.contains(license))
        {
            return Err(StoreError::LicenseRequired);
        }
        check_cancelled(&cancelled)?;
        self.initialize_schema()?;
        if hash_file_cancelled(archive_path, &cancelled)? != ARCHIVE_SHA256 {
            return Err(StoreError::Integrity);
        }
        check_cancelled(&cancelled)?;
        if let Ok(active) = self.active_model_dir()
            && verify_model_dir(&active).is_ok()
        {
            return Ok(());
        }

        self.publish_archive_cache(archive_path, &cancelled)?;
        check_cancelled(&cancelled)?;
        let versions = self.data_dir.join("models").join(MODEL_ID).join("versions");
        fs::create_dir_all(&versions).map_err(|_| StoreError::Io)?;
        let destination = versions.join(VERSION_TOKEN);
        if destination.exists() {
            verify_model_dir(&destination)?;
        } else {
            let staging = versions.join(format!(".staging-{}", operation_id()));
            fs::create_dir(&staging).map_err(|_| StoreError::Io)?;
            let result = extract_selected(archive_path, &staging, &cancelled).and_then(|()| {
                check_cancelled(&cancelled)?;
                let files = REQUIRED_FILES
                    .iter()
                    .map(|(name, hash)| ((*name).to_owned(), (*hash).to_owned()))
                    .collect();
                let manifest = ModelManifest {
                    schema_version: SCHEMA_VERSION,
                    model_id: MODEL_ID.to_owned(),
                    archive_sha256: ARCHIVE_SHA256.to_owned(),
                    accepted_licenses: accepted_licenses.to_vec(),
                    files,
                };
                write_json_synced(&staging.join("manifest.json"), &manifest)?;
                File::create(staging.join("lease.lock"))
                    .and_then(|file| file.sync_all())
                    .map_err(|_| StoreError::Io)?;
                sync_dir(&staging)?;
                check_cancelled(&cancelled)?;
                fs::rename(&staging, &destination).map_err(|_| StoreError::Io)?;
                sync_dir(&versions)
            });
            if result.is_err() {
                let _ = fs::remove_dir_all(&staging);
            }
            result?;
        }
        check_cancelled(&cancelled)?;
        publish_active(&self.data_dir.join("models").join(MODEL_ID), VERSION_TOKEN)
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
        self.import_voice_inner(
            source,
            voice_id,
            consent_confirmed,
            maximum_seconds,
            &mutation,
            || false,
        )
    }

    fn import_voice_inner<F>(
        &self,
        source: &Path,
        voice_id: &str,
        consent_confirmed: bool,
        maximum_seconds: f64,
        _mutation: &MutationGuard,
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
        check_cancelled(&cancelled)?;
        let reference =
            audio::read_reference(source, maximum_seconds).map_err(|_| StoreError::InvalidAudio)?;
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
                    model_id: MODEL_ID.to_owned(),
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
        self.import_voice_inner(
            source,
            voice_id,
            consent_confirmed,
            maximum_seconds,
            mutation,
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
        for artifact in artifacts {
            check_cancelled(&cancelled)?;
            let (root, version) = if artifact == &format!("model:{MODEL_ID}") {
                let root = self.data_dir.join("models").join(MODEL_ID);
                let version = read_active(&root)?;
                (root, version)
            } else if let Some(voice) = artifact.strip_prefix("voice:") {
                if !valid_voice_id(voice) {
                    return Err(StoreError::InvalidVoiceId);
                }
                let root = self.data_dir.join("voices").join(voice);
                let version = read_active(&root).map_err(|_| StoreError::VoiceMissing)?;
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
        Ok(artifacts.to_vec())
    }

    fn active_model_dir(&self) -> Result<PathBuf, StoreError> {
        let root = self.data_dir.join("models").join(MODEL_ID);
        let version = read_active(&root)?;
        if version != VERSION_TOKEN {
            return Err(StoreError::Integrity);
        }
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

    fn publish_archive_cache<F>(&self, source: &Path, cancelled: &F) -> Result<(), StoreError>
    where
        F: Fn() -> bool,
    {
        let destination = self
            .cache_dir
            .join("downloads")
            .join("sha256")
            .join(ARCHIVE_SHA256);
        if destination.exists() {
            return if hash_file_cancelled(&destination, cancelled)? == ARCHIVE_SHA256 {
                Ok(())
            } else {
                Err(StoreError::Integrity)
            };
        }
        let parent = destination.parent().ok_or(StoreError::Io)?;
        fs::create_dir_all(parent).map_err(|_| StoreError::Io)?;
        let temporary = parent.join(format!(".tmp-{}", operation_id()));
        let result = copy_file_cancelled(source, &temporary, cancelled);
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        check_cancelled(cancelled)?;
        match fs::rename(&temporary, &destination) {
            Ok(()) => sync_dir(parent),
            Err(_)
                if destination.exists()
                    && hash_file_cancelled(&destination, cancelled)? == ARCHIVE_SHA256 =>
            {
                let _ = fs::remove_file(temporary);
                Ok(())
            }
            Err(_) => Err(StoreError::Io),
        }
    }
}

fn extract_selected<F>(
    archive_path: &Path,
    destination: &Path,
    cancelled: &F,
) -> Result<(), StoreError>
where
    F: Fn() -> bool,
{
    let file = File::open(archive_path).map_err(|_| StoreError::Io)?;
    let mut archive = Archive::new(BzDecoder::new(file));
    let required: HashMap<_, _> = REQUIRED_FILES.iter().copied().collect();
    let mut found = HashSet::new();
    let mut total = 0_u64;
    let mut count = 0_usize;
    for entry in archive.entries().map_err(|_| StoreError::UnsafeArchive)? {
        check_cancelled(cancelled)?;
        let mut entry = entry.map_err(|_| StoreError::UnsafeArchive)?;
        count += 1;
        total = total.saturating_add(entry.size());
        if count > MAX_ARCHIVE_ENTRIES || total > MAX_EXTRACTED_BYTES {
            return Err(StoreError::UnsafeArchive);
        }
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(StoreError::UnsafeArchive);
        }
        let path = entry.path().map_err(|_| StoreError::UnsafeArchive)?;
        if path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
        {
            return Err(StoreError::UnsafeArchive);
        }
        if !kind.is_file() {
            continue;
        }
        let components: Vec<_> = path.components().collect();
        if components.len() != 2 {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Err(StoreError::UnsafeArchive);
        };
        let Some(expected) = required.get(name) else {
            continue;
        };
        if !found.insert(name.to_owned()) {
            return Err(StoreError::UnsafeArchive);
        }
        let output = destination.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .map_err(|_| StoreError::Io)?;
        copy_reader_cancelled(&mut entry, &mut file, cancelled)?;
        file.sync_all().map_err(|_| StoreError::Io)?;
        if hash_file_cancelled(&output, cancelled)? != *expected {
            return Err(StoreError::Integrity);
        }
    }
    if found.len() != required.len() {
        return Err(StoreError::Integrity);
    }
    Ok(())
}

fn verify_model_dir(path: &Path) -> Result<(), StoreError> {
    if !path.is_dir() {
        return Err(StoreError::ModelMissing);
    }
    let bytes = fs::read(path.join("manifest.json")).map_err(|_| StoreError::Integrity)?;
    let manifest: ModelManifest =
        serde_json::from_slice(&bytes).map_err(|_| StoreError::Integrity)?;
    if manifest.schema_version != SCHEMA_VERSION
        || manifest.model_id != MODEL_ID
        || manifest.archive_sha256 != ARCHIVE_SHA256
        || !crate::model::LICENSE_IDS.iter().all(|license| {
            manifest
                .accepted_licenses
                .iter()
                .any(|accepted| accepted == license)
        })
    {
        return Err(StoreError::Integrity);
    }
    for (name, expected) in REQUIRED_FILES {
        if manifest.files.get(*name).map(String::as_str) != Some(*expected)
            || hash_file(&path.join(name))? != *expected
        {
            return Err(StoreError::Integrity);
        }
    }
    Ok(())
}

fn read_voice_metadata(path: &Path) -> Result<VoiceMetadata, StoreError> {
    let bytes = fs::read(path.join("metadata.json")).map_err(|_| StoreError::Integrity)?;
    let metadata: VoiceMetadata =
        serde_json::from_slice(&bytes).map_err(|_| StoreError::Integrity)?;
    if metadata.schema_version != SCHEMA_VERSION {
        return Err(StoreError::Integrity);
    }
    Ok(metadata)
}

fn verify_voice_dir(path: &Path, expected_voice_id: &str) -> Result<(), StoreError> {
    let metadata = read_voice_metadata(path)?;
    if metadata.voice_id != expected_voice_id
        || metadata.model_id != MODEL_ID
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
    fn license_refusal_is_non_mutating() {
        let (temp, store) = test_store();
        let error = store
            .install_model_from_archive(Path::new("/does/not/exist"), &[])
            .unwrap_err();
        assert!(matches!(error, StoreError::LicenseRequired));
        assert!(!temp.path().join("data").exists());
        assert!(!temp.path().join("cache").exists());
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
