//! Strict manifest and file validation for native XN Pocket TTS bundles.

use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

use ptts::tts_model::TTSConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use thiserror::Error;

use crate::{engine::SAMPLE_RATE, xn_engine::XnModelBehavior};

pub const BUNDLE_SCHEMA: &str = "utterpipe.pocket-tts.xn-bundle/1";
pub const ENGINE_ID: &str = "xn-ptts";
pub const ENGINE_REVISION: &str = "4dbd8d6832cf4e093d08a1bd4666a08783345e7b";
pub const XN_VERSION: &str = "0.1.21";
pub const PRECISION: &str = "q8_0";
pub const APRIL_COMPATIBILITY: &str = "pocket-tts-english-2026-04";
pub const APRIL_MODEL_ID: &str = "pocket-tts-english-2026-04-q8";
pub const APRIL_SOURCE_REPOSITORY: &str = "kyutai/pocket-tts";
pub const APRIL_SOURCE_REVISION: &str = "19f95fe2df36e79fbd9f10008595cc4c977a0fcc";

const MANIFEST_NAME: &str = "manifest.json";
const CONFIG_NAME: &str = "config.json";
const MODEL_NAME: &str = "model.gguf";
const TOKENIZER_NAME: &str = "tokenizer.json";
const MAX_MANIFEST_BYTES: u64 = 64 * 1_024;
const MAX_CONFIG_BYTES: u64 = 1_048_576;
const MAX_MODEL_BYTES: u64 = 2 * 1_024 * 1_024 * 1_024;
const MAX_TOKENIZER_BYTES: u64 = 32 * 1_024 * 1_024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleFile {
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleBehavior {
    pub temperature: f32,
    pub output_gain: f32,
    pub pad_with_spaces_for_short_inputs: bool,
    pub remove_semicolons: bool,
    pub frames_after_eos_offset: usize,
}

impl From<&BundleBehavior> for XnModelBehavior {
    fn from(value: &BundleBehavior) -> Self {
        Self {
            temperature: value.temperature,
            output_gain: value.output_gain,
            pad_with_spaces_for_short_inputs: value.pad_with_spaces_for_short_inputs,
            remove_semicolons: value.remove_semicolons,
            frames_after_eos_offset: value.frames_after_eos_offset,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleRuntime {
    pub engine: String,
    pub revision: String,
    pub xn_version: String,
    pub precision: String,
    pub compatibility: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleLicense {
    pub id: String,
    pub name: String,
    pub url: String,
    pub requires_acceptance: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct XnBundleManifest {
    pub schema: String,
    pub model_id: String,
    pub name: String,
    pub version: String,
    pub languages: Vec<String>,
    pub source_repository: String,
    pub source_revision: String,
    pub runtime: BundleRuntime,
    pub behavior: BundleBehavior,
    pub licenses: Vec<BundleLicense>,
    pub files: BTreeMap<String, BundleFile>,
}

#[derive(Debug)]
pub struct VerifiedXnBundle {
    pub root: PathBuf,
    pub revision: String,
    pub manifest: XnBundleManifest,
}

impl VerifiedXnBundle {
    #[must_use]
    pub fn config_path(&self) -> PathBuf {
        self.root.join(CONFIG_NAME)
    }

    #[must_use]
    pub fn model_path(&self) -> PathBuf {
        self.root.join(MODEL_NAME)
    }

    #[must_use]
    pub fn tokenizer_path(&self) -> PathBuf {
        self.root.join(TOKENIZER_NAME)
    }

    #[must_use]
    pub fn behavior(&self) -> XnModelBehavior {
        (&self.manifest.behavior).into()
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum XnBundleError {
    #[error("XN model bundle path is invalid")]
    InvalidPath,
    #[error("XN model bundle manifest is invalid or unsupported")]
    InvalidManifest,
    #[error("XN model bundle file failed integrity validation")]
    Integrity,
    #[error("XN model bundle validation was cancelled")]
    Cancelled,
    #[error("XN model bundle could not be read")]
    Io,
}

/// Validate a complete extracted XN model bundle without loading model weights.
///
/// # Errors
///
/// Returns a stable path, schema, integrity, cancellation, or I/O error. The
/// native model loader remains the final compatibility authority before an
/// installed bundle is activated.
pub fn verify_bundle(
    root: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<VerifiedXnBundle, XnBundleError> {
    if !root.is_absolute() {
        return Err(XnBundleError::InvalidPath);
    }
    let root_metadata = fs::symlink_metadata(root).map_err(|_| XnBundleError::Io)?;
    if !root_metadata.file_type().is_dir() {
        return Err(XnBundleError::InvalidPath);
    }
    check_cancelled(&cancelled)?;
    let manifest_path = root.join(MANIFEST_NAME);
    let bytes = read_bounded_regular(&manifest_path, MAX_MANIFEST_BYTES, &cancelled)?;
    let manifest: XnBundleManifest =
        serde_json::from_slice(&bytes).map_err(|_| XnBundleError::InvalidManifest)?;
    validate_manifest(&manifest)?;

    let config = verified_small_file(root, &manifest, CONFIG_NAME, MAX_CONFIG_BYTES, &cancelled)?;
    let tokenizer = verified_small_file(
        root,
        &manifest,
        TOKENIZER_NAME,
        MAX_TOKENIZER_BYTES,
        &cancelled,
    )?;
    check_cancelled(&cancelled)?;
    let expected_model = manifest
        .files
        .get(MODEL_NAME)
        .ok_or(XnBundleError::InvalidManifest)?;
    if expected_model.bytes == 0 || expected_model.bytes > MAX_MODEL_BYTES {
        return Err(XnBundleError::InvalidManifest);
    }
    let actual_model =
        hash_bounded_regular(&root.join(MODEL_NAME), expected_model.bytes, &cancelled)?;
    if actual_model != expected_model.sha256 {
        return Err(XnBundleError::Integrity);
    }

    check_cancelled(&cancelled)?;
    let config: TTSConfig =
        serde_json::from_slice(&config).map_err(|_| XnBundleError::InvalidManifest)?;
    if config.mimi.sample_rate != SAMPLE_RATE as usize
        || !config.temp.is_finite()
        || (config.temp - manifest.behavior.temperature).abs() > f32::EPSILON
    {
        return Err(XnBundleError::InvalidManifest);
    }
    tokenizers::Tokenizer::from_bytes(tokenizer).map_err(|_| XnBundleError::InvalidManifest)?;

    let canonical =
        serde_json_canonicalizer::to_vec(&manifest).map_err(|_| XnBundleError::InvalidManifest)?;
    let revision = format!("{:x}", Sha256::digest(canonical));
    Ok(VerifiedXnBundle {
        root: root.to_owned(),
        revision,
        manifest,
    })
}

fn verified_small_file<F>(
    root: &Path,
    manifest: &XnBundleManifest,
    name: &str,
    maximum: u64,
    cancelled: &F,
) -> Result<Vec<u8>, XnBundleError>
where
    F: Fn() -> bool,
{
    check_cancelled(cancelled)?;
    let expected = manifest
        .files
        .get(name)
        .ok_or(XnBundleError::InvalidManifest)?;
    if expected.bytes == 0 || expected.bytes > maximum {
        return Err(XnBundleError::InvalidManifest);
    }
    let bytes = read_bounded_regular(&root.join(name), expected.bytes, cancelled)?;
    if format!("{:x}", Sha256::digest(&bytes)) != expected.sha256 {
        return Err(XnBundleError::Integrity);
    }
    Ok(bytes)
}

fn validate_manifest(manifest: &XnBundleManifest) -> Result<(), XnBundleError> {
    if manifest.schema != BUNDLE_SCHEMA
        || !valid_id(&manifest.model_id)
        || manifest.name.is_empty()
        || manifest.name.chars().count() > 128
        || manifest.version.is_empty()
        || manifest.version.len() > 64
        || manifest.languages != ["en"]
        || manifest.source_repository != APRIL_SOURCE_REPOSITORY
        || manifest.source_revision != APRIL_SOURCE_REVISION
        || manifest.runtime.engine != ENGINE_ID
        || manifest.runtime.revision != ENGINE_REVISION
        || manifest.runtime.xn_version != XN_VERSION
        || manifest.runtime.precision != PRECISION
        || manifest.runtime.compatibility != APRIL_COMPATIBILITY
        || manifest.model_id != APRIL_MODEL_ID
        || !valid_behavior(&manifest.behavior)
        || manifest.files.len() != 3
    {
        return Err(XnBundleError::InvalidManifest);
    }
    let required: HashSet<_> = [CONFIG_NAME, MODEL_NAME, TOKENIZER_NAME]
        .into_iter()
        .collect();
    if manifest
        .files
        .keys()
        .map(String::as_str)
        .collect::<HashSet<_>>()
        != required
        || manifest
            .files
            .values()
            .any(|file| !valid_hex_digest(&file.sha256))
        || manifest.licenses.is_empty()
        || manifest.licenses.len() > 16
    {
        return Err(XnBundleError::InvalidManifest);
    }
    let mut license_ids = HashSet::new();
    for license in &manifest.licenses {
        if !valid_id(&license.id)
            || !license_ids.insert(&license.id)
            || license.name.is_empty()
            || license.name.chars().count() > 128
            || !license.requires_acceptance
            || url::Url::parse(&license.url)
                .ok()
                .is_none_or(|url| url.scheme() != "https")
        {
            return Err(XnBundleError::InvalidManifest);
        }
    }
    for (id, url) in [
        ("cc-by-4.0", "https://creativecommons.org/licenses/by/4.0/"),
        (
            "pocket-tts-acceptable-use",
            "https://huggingface.co/kyutai/pocket-tts",
        ),
    ] {
        if !manifest
            .licenses
            .iter()
            .any(|license| license.id == id && license.url == url)
        {
            return Err(XnBundleError::InvalidManifest);
        }
    }
    Ok(())
}

fn valid_behavior(behavior: &BundleBehavior) -> bool {
    behavior.temperature.is_finite()
        && behavior.temperature > 0.0
        && behavior.temperature <= 4.0
        && behavior.output_gain.is_finite()
        && behavior.output_gain > 0.0
        && behavior.output_gain <= 1.0
        && behavior.frames_after_eos_offset <= 64
}

fn valid_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    let edge = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    (1..=128).contains(&bytes.len())
        && bytes.first().is_some_and(edge)
        && bytes.last().is_some_and(edge)
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn valid_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn read_bounded_regular<F>(
    path: &Path,
    maximum: u64,
    cancelled: &F,
) -> Result<Vec<u8>, XnBundleError>
where
    F: Fn() -> bool,
{
    let (mut file, metadata) = open_regular(path)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > maximum {
        return Err(XnBundleError::Integrity);
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| XnBundleError::Integrity)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| XnBundleError::Integrity)?;
    let mut buffer = [0_u8; 64 * 1_024];
    loop {
        check_cancelled(cancelled)?;
        let count = file.read(&mut buffer).map_err(|_| XnBundleError::Io)?;
        if count == 0 {
            break;
        }
        if bytes.len().saturating_add(count) > capacity {
            return Err(XnBundleError::Integrity);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    if bytes.len() != capacity {
        return Err(XnBundleError::Integrity);
    }
    Ok(bytes)
}

fn hash_bounded_regular<F>(
    path: &Path,
    expected_bytes: u64,
    cancelled: &F,
) -> Result<String, XnBundleError>
where
    F: Fn() -> bool,
{
    let (mut file, metadata) = open_regular(path)?;
    if !metadata.file_type().is_file() || metadata.len() != expected_bytes {
        return Err(XnBundleError::Integrity);
    }
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1_024];
    loop {
        check_cancelled(cancelled)?;
        let count = file.read(&mut buffer).map_err(|_| XnBundleError::Io)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or(XnBundleError::Integrity)?;
        if total > expected_bytes {
            return Err(XnBundleError::Integrity);
        }
        digest.update(&buffer[..count]);
    }
    if total != expected_bytes {
        return Err(XnBundleError::Integrity);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn open_regular(path: &Path) -> Result<(File, fs::Metadata), XnBundleError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| XnBundleError::Io)?;
    if !metadata.file_type().is_file() {
        return Err(XnBundleError::Integrity);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options.open(path).map_err(|_| XnBundleError::Io)?;
    let opened = file.metadata().map_err(|_| XnBundleError::Io)?;
    if !opened.file_type().is_file() || opened.len() != metadata.len() {
        return Err(XnBundleError::Integrity);
    }
    Ok((file, opened))
}

fn check_cancelled(cancelled: &impl Fn() -> bool) -> Result<(), XnBundleError> {
    if cancelled() {
        Err(XnBundleError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn fixture() -> (TempDir, XnBundleManifest) {
        let temp = tempfile::tempdir().unwrap();
        let config = serde_json::to_vec(&TTSConfig::v202601(0.3)).unwrap();
        let model = b"not loaded by structural verification";
        let tokenizer = br#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":null,"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"test":0},"unk_token":"test"}}"#;
        fs::write(temp.path().join(CONFIG_NAME), &config).unwrap();
        fs::write(temp.path().join(MODEL_NAME), model).unwrap();
        fs::write(temp.path().join(TOKENIZER_NAME), tokenizer).unwrap();
        let files = BTreeMap::from([
            (
                CONFIG_NAME.to_owned(),
                BundleFile {
                    bytes: config.len() as u64,
                    sha256: digest(&config),
                },
            ),
            (
                MODEL_NAME.to_owned(),
                BundleFile {
                    bytes: model.len() as u64,
                    sha256: digest(model),
                },
            ),
            (
                TOKENIZER_NAME.to_owned(),
                BundleFile {
                    bytes: tokenizer.len() as u64,
                    sha256: digest(tokenizer),
                },
            ),
        ]);
        let manifest = XnBundleManifest {
            schema: BUNDLE_SCHEMA.to_owned(),
            model_id: APRIL_MODEL_ID.to_owned(),
            name: "Pocket TTS English April 2026 Q8".to_owned(),
            version: "2026-04".to_owned(),
            languages: vec!["en".to_owned()],
            source_repository: APRIL_SOURCE_REPOSITORY.to_owned(),
            source_revision: APRIL_SOURCE_REVISION.to_owned(),
            runtime: BundleRuntime {
                engine: ENGINE_ID.to_owned(),
                revision: ENGINE_REVISION.to_owned(),
                xn_version: XN_VERSION.to_owned(),
                precision: PRECISION.to_owned(),
                compatibility: APRIL_COMPATIBILITY.to_owned(),
            },
            behavior: BundleBehavior {
                temperature: 0.3,
                output_gain: 0.65,
                pad_with_spaces_for_short_inputs: false,
                remove_semicolons: false,
                frames_after_eos_offset: 2,
            },
            licenses: vec![
                BundleLicense {
                    id: "cc-by-4.0".to_owned(),
                    name: "Creative Commons Attribution 4.0".to_owned(),
                    url: "https://creativecommons.org/licenses/by/4.0/".to_owned(),
                    requires_acceptance: true,
                },
                BundleLicense {
                    id: "pocket-tts-acceptable-use".to_owned(),
                    name: "Pocket TTS prohibited-use conditions".to_owned(),
                    url: "https://huggingface.co/kyutai/pocket-tts".to_owned(),
                    requires_acceptance: true,
                },
            ],
            files,
        };
        fs::write(
            temp.path().join(MANIFEST_NAME),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        (temp, manifest)
    }

    #[test]
    fn verifies_exact_files_and_returns_a_stable_revision() {
        let (temp, manifest) = fixture();
        let verified = verify_bundle(temp.path(), || false).unwrap();
        assert_eq!(verified.manifest, manifest);
        assert!(valid_hex_digest(&verified.revision));
        assert_eq!(verified.behavior(), (&manifest.behavior).into());
    }

    #[test]
    fn rejects_tampering_unknown_fields_and_relative_roots() {
        let (temp, _) = fixture();
        fs::write(temp.path().join(MODEL_NAME), b"tampered").unwrap();
        assert_eq!(
            verify_bundle(temp.path(), || false).unwrap_err(),
            XnBundleError::Integrity
        );

        let (temp, _) = fixture();
        let path = temp.path().join(MANIFEST_NAME);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["unexpected"] = serde_json::json!(true);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            verify_bundle(temp.path(), || false).unwrap_err(),
            XnBundleError::InvalidManifest
        );
        assert_eq!(
            verify_bundle(Path::new("relative"), || false).unwrap_err(),
            XnBundleError::InvalidPath
        );
    }

    #[test]
    fn cancellation_precedes_large_file_hashing() {
        let (temp, _) = fixture();
        assert_eq!(
            verify_bundle(temp.path(), || true).unwrap_err(),
            XnBundleError::Cancelled
        );
    }
}
