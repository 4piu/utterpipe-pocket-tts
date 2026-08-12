//! Authenticated bootstrap and deterministic Q8 conversion for the pinned XN model.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use xn::quantized::{GgmlDType, QStorage, QTensor, gguf_file};
use xn::{CPU, TypedTensor, safetensors};

use crate::xn_bundle::{
    APRIL_COMPATIBILITY, APRIL_MODEL_ID, APRIL_SOURCE_REPOSITORY, APRIL_SOURCE_REVISION,
    BUNDLE_SCHEMA, BundleBehavior, BundleFile, BundleLicense, BundleRuntime, ENGINE_ID,
    ENGINE_REVISION, PRECISION, XN_VERSION, XnBundleManifest,
};

pub const SOURCE_MODEL_PATH: &str = "languages/english_2026-04/model.safetensors";
pub const SOURCE_MODEL_BYTES: u64 = 219_029_196;
pub const SOURCE_MODEL_SHA256: &str =
    "473f47d99560bd50eb8b4509d3cacfe7f316ab20bdca86505403a2e6a936a6e9";
pub const SOURCE_TOKENIZER_PATH: &str = "languages/english_2026-04/tokenizer.model";
pub const SOURCE_TOKENIZER_BYTES: u64 = 59_339;
pub const SOURCE_TOKENIZER_SHA256: &str =
    "d461765ae179566678c93091c5fa6f2984c31bbe990bf1aa62d92c64d91bc3f6";
pub const CONFIG_BYTES: u64 = 1_279;
pub const CONFIG_SHA256: &str = "10cf232cb3bbefa3862da21fb5d051f8c76fb9abbcfa7f2357f5a19c917ee535";
pub const RUNTIME_MODEL_BYTES: u64 = 148_242_752;
pub const RUNTIME_MODEL_SHA256: &str =
    "a9548b363f990faca0614dc0533d80b11be80ad0b6ac781b6f42a58dd1659ece";
pub const DOWNLOAD_BYTES: u64 = SOURCE_MODEL_BYTES + SOURCE_TOKENIZER_BYTES;
pub const INSTALLED_BYTES: u64 = RUNTIME_MODEL_BYTES + SOURCE_TOKENIZER_BYTES + CONFIG_BYTES;

const CONFIG: &[u8] = include_bytes!("../assets/pocket-tts-english-2026-04-config.json");
const MAX_TOKEN_BYTES: u64 = 16 * 1_024;
static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
struct SourceFile {
    remote_path: &'static str,
    local_name: &'static str,
    bytes: u64,
    sha256: &'static str,
}

const SOURCE_FILES: &[SourceFile] = &[
    SourceFile {
        remote_path: SOURCE_MODEL_PATH,
        local_name: "model.safetensors",
        bytes: SOURCE_MODEL_BYTES,
        sha256: SOURCE_MODEL_SHA256,
    },
    SourceFile {
        remote_path: SOURCE_TOKENIZER_PATH,
        local_name: "tokenizer.model",
        bytes: SOURCE_TOKENIZER_BYTES,
        sha256: SOURCE_TOKENIZER_SHA256,
    },
];

#[derive(Debug, Error)]
pub enum PrepareError {
    #[error(
        "Hugging Face authentication is required; set HF_TOKEN or sign in with a compatible Hugging Face client"
    )]
    AuthenticationRequired,
    #[error("the pinned Pocket TTS source download failed")]
    Network,
    #[error("the pinned Pocket TTS source failed integrity validation")]
    Integrity,
    #[error("model preparation was cancelled")]
    Cancelled,
    #[error("model preparation storage is unavailable")]
    Storage,
    #[error("the pinned Pocket TTS model could not be converted")]
    Conversion,
}

pub struct PreparedBundle {
    root: PathBuf,
}

impl PreparedBundle {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for PreparedBundle {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Resolve the normal Hugging Face token environment or cache file without
/// ever including the credential in diagnostics.
#[must_use]
pub fn resolve_hf_token() -> Option<String> {
    if let Some(token) = std::env::var_os("HF_TOKEN") {
        return valid_token(token.to_string_lossy().into_owned());
    }
    let explicit = std::env::var_os("HF_TOKEN_PATH").map(PathBuf::from);
    let path = explicit.or_else(default_token_path)?;
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > MAX_TOKEN_BYTES {
        return None;
    }
    let mut token = String::new();
    File::open(path)
        .ok()?
        .take(MAX_TOKEN_BYTES + 1)
        .read_to_string(&mut token)
        .ok()?;
    valid_token(token)
}

fn valid_token(token: String) -> Option<String> {
    let token = token.trim();
    (!token.is_empty() && !token.chars().any(char::is_whitespace)).then(|| token.to_owned())
}

fn default_token_path() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("HF_HOME") {
        return Some(PathBuf::from(root).join("token"));
    }
    if let Some(root) = std::env::var_os("XDG_CACHE_HOME") {
        return Some(PathBuf::from(root).join("huggingface").join("token"));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|root| root.join(".cache").join("huggingface").join("token"))
}

/// Download or verify the pinned official source, convert it to the accepted
/// Q8 profile, and return a temporary self-describing bundle.
pub async fn prepare_bundle(
    cache_dir: &Path,
    source_dir: Option<&Path>,
    cancellation: CancellationToken,
) -> Result<PreparedBundle, PrepareError> {
    let root = private_temporary_directory(cache_dir)?;
    let prepared = PreparedBundle { root };
    let sources = prepared.path().join("source");
    create_private_dir(&sources)?;

    if let Some(source_dir) = source_dir {
        if !source_dir.is_absolute() {
            return Err(PrepareError::Storage);
        }
        for source in SOURCE_FILES {
            copy_verified(
                &source_dir.join(source.local_name),
                &sources.join(source.local_name),
                source,
                &cancellation,
            )?;
        }
    } else {
        let token = resolve_hf_token().ok_or(PrepareError::AuthenticationRequired)?;
        for source in SOURCE_FILES {
            download_source(cache_dir, &sources, source, &token, &cancellation).await?;
        }
    }

    check_cancelled(&cancellation)?;
    write_private(prepared.path().join("config.json"), CONFIG)?;
    if hash_file(&prepared.path().join("config.json"), &cancellation)? != CONFIG_SHA256 {
        return Err(PrepareError::Integrity);
    }
    copy_verified(
        &sources.join("tokenizer.model"),
        &prepared.path().join("tokenizer.model"),
        &SOURCE_FILES[1],
        &cancellation,
    )?;

    let input = sources.join("model.safetensors");
    let output = prepared.path().join("model.gguf");
    let task_cancellation = cancellation.clone();
    tokio::task::spawn_blocking(move || quantize_q8(&input, &output, &task_cancellation))
        .await
        .map_err(|_| PrepareError::Conversion)??;
    if fs::metadata(prepared.path().join("model.gguf"))
        .map_err(|_| PrepareError::Storage)?
        .len()
        != RUNTIME_MODEL_BYTES
        || hash_file(&prepared.path().join("model.gguf"), &cancellation)? != RUNTIME_MODEL_SHA256
    {
        return Err(PrepareError::Integrity);
    }
    fs::remove_dir_all(&sources).map_err(|_| PrepareError::Storage)?;

    let manifest = catalog_manifest();
    let encoded = serde_json::to_vec_pretty(&manifest).map_err(|_| PrepareError::Storage)?;
    write_private(prepared.path().join("manifest.json"), &encoded)?;
    check_cancelled(&cancellation)?;
    Ok(prepared)
}

#[must_use]
pub fn catalog_manifest() -> XnBundleManifest {
    XnBundleManifest {
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
        files: BTreeMap::from([
            (
                "config.json".to_owned(),
                BundleFile {
                    bytes: CONFIG_BYTES,
                    sha256: CONFIG_SHA256.to_owned(),
                },
            ),
            (
                "model.gguf".to_owned(),
                BundleFile {
                    bytes: RUNTIME_MODEL_BYTES,
                    sha256: RUNTIME_MODEL_SHA256.to_owned(),
                },
            ),
            (
                "tokenizer.model".to_owned(),
                BundleFile {
                    bytes: SOURCE_TOKENIZER_BYTES,
                    sha256: SOURCE_TOKENIZER_SHA256.to_owned(),
                },
            ),
        ]),
    }
}

async fn download_source(
    cache_dir: &Path,
    destination_dir: &Path,
    source: &SourceFile,
    token: &str,
    cancellation: &CancellationToken,
) -> Result<(), PrepareError> {
    let cache = cache_dir
        .join("downloads")
        .join("sha256")
        .join(source.sha256);
    if cache.is_file() && hash_file(&cache, cancellation)? == source.sha256 {
        return copy_verified(
            &cache,
            &destination_dir.join(source.local_name),
            source,
            cancellation,
        );
    }
    let parent = cache.parent().ok_or(PrepareError::Storage)?;
    fs::create_dir_all(parent).map_err(|_| PrepareError::Storage)?;
    let temporary = parent.join(format!(
        ".xn-download-{}-{}",
        std::process::id(),
        OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _cleanup = FileCleanup(temporary.clone());
    let client = reqwest::Client::builder()
        .tls_backend_rustls()
        .redirect(reqwest::redirect::Policy::limited(10))
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| PrepareError::Network)?;
    let url = format!(
        "https://huggingface.co/{APRIL_SOURCE_REPOSITORY}/resolve/{APRIL_SOURCE_REVISION}/{}?download=true",
        source.remote_path
    );
    let operation = async {
        let response = client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| PrepareError::Network)?;
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err(PrepareError::AuthenticationRequired);
        }
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|bytes| bytes != source.bytes)
        {
            return Err(PrepareError::Network);
        }
        let standard = open_private(&temporary)?;
        let mut file = tokio::fs::File::from_std(standard);
        let mut stream = response.bytes_stream();
        let mut total = 0_u64;
        let mut digest = Sha256::new();
        while let Some(next) = tokio::select! {
            () = cancellation.cancelled() => return Err(PrepareError::Cancelled),
            next = stream.next() => next,
        } {
            let chunk = next.map_err(|_| PrepareError::Network)?;
            total = total
                .checked_add(chunk.len() as u64)
                .ok_or(PrepareError::Integrity)?;
            if total > source.bytes {
                return Err(PrepareError::Integrity);
            }
            digest.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|_| PrepareError::Storage)?;
        }
        file.sync_all().await.map_err(|_| PrepareError::Storage)?;
        if total != source.bytes || format!("{:x}", digest.finalize()) != source.sha256 {
            return Err(PrepareError::Integrity);
        }
        Ok(())
    };
    tokio::select! {
        () = cancellation.cancelled() => Err(PrepareError::Cancelled),
        result = tokio::time::timeout(Duration::from_secs(1_800), operation) => {
            result.map_err(|_| PrepareError::Network)?
        }
    }?;
    if cache.exists() {
        let metadata = fs::symlink_metadata(&cache).map_err(|_| PrepareError::Storage)?;
        if metadata.file_type().is_file() && hash_file(&cache, cancellation)? == source.sha256 {
            // Another process published the same verified content first.
        } else {
            fs::remove_file(&cache).map_err(|_| PrepareError::Storage)?;
            fs::rename(&temporary, &cache).map_err(|_| PrepareError::Storage)?;
        }
    } else {
        fs::rename(&temporary, &cache).map_err(|_| PrepareError::Storage)?;
    }
    copy_verified(
        &cache,
        &destination_dir.join(source.local_name),
        source,
        cancellation,
    )
}

fn quantize_q8(
    input: &Path,
    output: &Path,
    cancellation: &CancellationToken,
) -> Result<(), PrepareError> {
    check_cancelled(cancellation)?;
    let tensors = safetensors::load_from_file(input, &CPU).map_err(|_| PrepareError::Conversion)?;
    let mut names: Vec<&String> = tensors.keys().collect();
    names.sort_unstable();
    let mut converted = Vec::with_capacity(names.len());
    for name in names {
        check_cancelled(cancellation)?;
        if excluded_weight(name) {
            continue;
        }
        let tensor = &tensors[name];
        let converted_tensor = if quantized_weight(name) {
            let values = tensor_to_f32(tensor)?;
            QTensor::quantize_f32(&values, tensor.shape(), GgmlDType::Q8_0)
                .map_err(|_| PrepareError::Conversion)?
        } else {
            as_is_qtensor(tensor)?
        };
        converted.push((name.clone(), converted_tensor));
    }
    check_cancelled(cancellation)?;
    let refs: Vec<(&str, &QTensor)> = converted
        .iter()
        .map(|(name, tensor)| (name.as_str(), tensor))
        .collect();
    let file = open_private(output)?;
    let mut writer = BufWriter::new(file);
    gguf_file::write(&mut writer, &[], &refs).map_err(|_| PrepareError::Conversion)?;
    writer.flush().map_err(|_| PrepareError::Storage)?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|_| PrepareError::Storage)?;
    check_cancelled(cancellation)
}

fn excluded_weight(name: &str) -> bool {
    name.starts_with("mimi.quantizer.") && !name.starts_with("mimi.quantizer.output_proj")
}

fn quantized_weight(name: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        "linear1.weight",
        "linear2.weight",
        "self_attn.in_proj.weight",
        "self_attn.out_proj.weight",
    ];
    name.starts_with("flow_lm.transformer.layers.")
        && SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

fn tensor_to_f32(tensor: &TypedTensor<xn::CpuDevice>) -> Result<Vec<f32>, PrepareError> {
    match tensor {
        TypedTensor::F32(value) => value.to_vec().map_err(|_| PrepareError::Conversion),
        TypedTensor::F16(value) => value
            .to_vec()
            .map(|items| items.into_iter().map(|item| item.to_f32()).collect())
            .map_err(|_| PrepareError::Conversion),
        TypedTensor::BF16(value) => value
            .to_vec()
            .map(|items| items.into_iter().map(|item| item.to_f32()).collect())
            .map_err(|_| PrepareError::Conversion),
        TypedTensor::I64(_) | TypedTensor::U8(_) => Err(PrepareError::Conversion),
    }
}

fn as_is_qtensor(tensor: &TypedTensor<xn::CpuDevice>) -> Result<QTensor, PrepareError> {
    let shape = tensor.shape().clone();
    let storage = match tensor {
        TypedTensor::F32(value) => QStorage::Cpu(Box::new(
            value.to_vec().map_err(|_| PrepareError::Conversion)?,
        )),
        TypedTensor::F16(value) => QStorage::Cpu(Box::new(
            value.to_vec().map_err(|_| PrepareError::Conversion)?,
        )),
        TypedTensor::BF16(value) => QStorage::Cpu(Box::new(
            value.to_vec().map_err(|_| PrepareError::Conversion)?,
        )),
        TypedTensor::I64(_) | TypedTensor::U8(_) => return Err(PrepareError::Conversion),
    };
    QTensor::new(storage, shape).map_err(|_| PrepareError::Conversion)
}

fn private_temporary_directory(cache_dir: &Path) -> Result<PathBuf, PrepareError> {
    let root = cache_dir.join("tmp");
    fs::create_dir_all(&root).map_err(|_| PrepareError::Storage)?;
    let path = root.join(format!(
        "xn-prepare-{}-{}",
        std::process::id(),
        OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    create_private_dir(&path)?;
    Ok(path)
}

fn create_private_dir(path: &Path) -> Result<(), PrepareError> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path).map_err(|_| PrepareError::Storage)
}

fn open_private(path: &Path) -> Result<File, PrepareError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path).map_err(|_| PrepareError::Storage)
}

fn write_private(path: PathBuf, bytes: &[u8]) -> Result<(), PrepareError> {
    let mut file = open_private(&path)?;
    file.write_all(bytes).map_err(|_| PrepareError::Storage)?;
    file.sync_all().map_err(|_| PrepareError::Storage)
}

fn copy_verified(
    source_path: &Path,
    destination: &Path,
    expected: &SourceFile,
    cancellation: &CancellationToken,
) -> Result<(), PrepareError> {
    let metadata = fs::symlink_metadata(source_path).map_err(|_| PrepareError::Storage)?;
    if !metadata.file_type().is_file() || metadata.len() != expected.bytes {
        return Err(PrepareError::Integrity);
    }
    let mut source = File::open(source_path).map_err(|_| PrepareError::Storage)?;
    let mut target = open_private(destination)?;
    let mut buffer = [0_u8; 64 * 1_024];
    let mut digest = Sha256::new();
    loop {
        check_cancelled(cancellation)?;
        let count = source
            .read(&mut buffer)
            .map_err(|_| PrepareError::Storage)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        target
            .write_all(&buffer[..count])
            .map_err(|_| PrepareError::Storage)?;
    }
    target.sync_all().map_err(|_| PrepareError::Storage)?;
    if format!("{:x}", digest.finalize()) != expected.sha256 {
        return Err(PrepareError::Integrity);
    }
    Ok(())
}

fn hash_file(path: &Path, cancellation: &CancellationToken) -> Result<String, PrepareError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| PrepareError::Storage)?;
    if !metadata.file_type().is_file() {
        return Err(PrepareError::Integrity);
    }
    let mut file = File::open(path).map_err(|_| PrepareError::Storage)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1_024];
    loop {
        check_cancelled(cancellation)?;
        let count = file.read(&mut buffer).map_err(|_| PrepareError::Storage)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<(), PrepareError> {
    if cancellation.is_cancelled() {
        Err(PrepareError::Cancelled)
    } else {
        Ok(())
    }
}

struct FileCleanup(PathBuf);

impl Drop for FileCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_config_and_catalog_are_self_consistent() {
        assert_eq!(CONFIG.len() as u64, CONFIG_BYTES);
        assert_eq!(format!("{:x}", Sha256::digest(CONFIG)), CONFIG_SHA256);
        let manifest = catalog_manifest();
        assert_eq!(manifest.model_id, APRIL_MODEL_ID);
        assert_eq!(manifest.behavior.output_gain, 0.65);
        assert_eq!(manifest.files["model.gguf"].sha256, RUNTIME_MODEL_SHA256);
        assert_eq!(
            manifest.files["tokenizer.model"].bytes,
            SOURCE_TOKENIZER_BYTES
        );
    }

    #[test]
    fn token_validation_never_accepts_whitespace_or_empty_values() {
        assert_eq!(
            valid_token("  token-value\n".to_owned()).as_deref(),
            Some("token-value")
        );
        assert!(valid_token("".to_owned()).is_none());
        assert!(valid_token("two words".to_owned()).is_none());
    }
}
