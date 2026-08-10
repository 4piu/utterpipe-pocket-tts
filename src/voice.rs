use std::{
    ffi::OsString,
    fs::OpenOptions,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::audio::{LARGE_REFERENCE_WARNING_BYTES, MAX_RIFF_REFERENCE_BYTES};

pub const CURATED_REPOSITORY: &str = "kyutai/tts-voices";
pub const CURATED_REVISION: &str = "323332d33f997de8394f24a193e1a76df720e01a";
pub const CURATED_LICENSE_ID: &str = "cc0-1.0";
pub const CURATED_LICENSE_NAME: &str = "Creative Commons CC0 1.0 Universal";
pub const CURATED_LICENSE_URL: &str = "https://creativecommons.org/publicdomain/zero/1.0/";
pub const CURATED_SOURCE_URL: &str = "https://huggingface.co/kyutai/tts-voices";

static DOWNLOAD_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Serialize)]
pub struct CuratedVoice {
    pub id: &'static str,
    pub name: &'static str,
    pub repository: &'static str,
    pub revision: &'static str,
    pub path: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
    pub license_id: &'static str,
    pub license_name: &'static str,
    pub license_url: &'static str,
    pub attribution: &'static str,
}

pub const CURATED_VOICES: &[CuratedVoice] = &[
    curated_voice(
        "voice-zero-bill-boerst",
        "Bill Boerst",
        "voice-zero/bill_boerst.wav",
        955_496,
        "be4815e4fb760ba1b78117545a260cce4a4c124c7657bc5c6127a0fef8ba661f",
    ),
    curated_voice(
        "voice-zero-caro-davy",
        "Caro Davy",
        "voice-zero/caro_davy.wav",
        743_528,
        "40c692c005a0268a7a5b6ebae348077d3dca6a86eb6b12bd36e343bbcd71b5f6",
    ),
    curated_voice(
        "voice-zero-peter-yearsley",
        "Peter Yearsley",
        "voice-zero/peter_yearsley.wav",
        524_448,
        "fbb3920fda7ae26a5a8b317ffcae1d55c0bd5d89d075205f5a52b1e924b83f51",
    ),
    curated_voice(
        "voice-zero-stuart-bell",
        "Stuart Bell",
        "voice-zero/stuart_bell.wav",
        745_776,
        "00c7baeb2fb7a8c1c6198e045b5e853a7ccc04002a51a09b4be3dd7c96994f73",
    ),
];

const fn curated_voice(
    id: &'static str,
    name: &'static str,
    path: &'static str,
    bytes: u64,
    sha256: &'static str,
) -> CuratedVoice {
    CuratedVoice {
        id,
        name,
        repository: CURATED_REPOSITORY,
        revision: CURATED_REVISION,
        path,
        bytes,
        sha256,
        license_id: CURATED_LICENSE_ID,
        license_name: CURATED_LICENSE_NAME,
        license_url: CURATED_LICENSE_URL,
        attribution: "Voice-Zero, curated from LibriVox and published by Kyutai under CC0",
    }
}

#[must_use]
pub fn curated_voice_by_id(id: &str) -> Option<&'static CuratedVoice> {
    CURATED_VOICES.iter().find(|voice| voice.id == id)
}

#[derive(Debug)]
pub enum VoiceSource {
    File(PathBuf),
    Url(Url),
}

impl VoiceSource {
    /// Classify only explicit HTTP(S) prefixes as network sources. Everything
    /// else remains a platform path, including Windows drive-letter paths.
    ///
    /// # Errors
    ///
    /// Returns an error when an explicit HTTP(S) URL is malformed.
    pub fn parse(value: OsString) -> Result<Self, VoiceDownloadError> {
        let text = value.to_string_lossy();
        let explicit_http = text
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
            || text
                .get(..8)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"));
        if explicit_http {
            let url = Url::parse(&text).map_err(|_| VoiceDownloadError::InvalidUrl)?;
            return Ok(Self::Url(url));
        }
        Ok(Self::File(PathBuf::from(value)))
    }
}

#[derive(Clone, Copy)]
pub struct ExpectedDownload {
    pub bytes: u64,
    pub sha256: &'static str,
}

pub struct StagedVoice {
    path: PathBuf,
    pub bytes: u64,
}

impl StagedVoice {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagedVoice {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Debug, Error)]
pub enum VoiceDownloadError {
    #[error("voice source URL is invalid")]
    InvalidUrl,
    #[error("voice download was cancelled")]
    Cancelled,
    #[error("voice download timed out")]
    Timeout,
    #[error("voice download failed")]
    Network,
    #[error("voice source returned an error")]
    Status,
    #[error("voice download could not be staged")]
    Storage,
    #[error("voice download exceeds the classic RIFF/WAVE format size")]
    TooLarge,
    #[error("curated voice download failed its pinned size or checksum")]
    Integrity,
}

/// Download one HTTP(S) source into private provider-cache staging.
///
/// The body is streamed with bounded memory. `large_warning` is invoked once
/// when either the declared or observed size crosses the diagnostic threshold.
///
/// # Errors
///
/// Returns a stable download, cancellation, storage, or integrity error.
pub async fn download_voice(
    url: Url,
    cache_dir: &Path,
    expected: Option<ExpectedDownload>,
    cancellation: CancellationToken,
    mut large_warning: impl FnMut(u64),
) -> Result<StagedVoice, VoiceDownloadError> {
    let temporary_root = cache_dir.join("tmp");
    tokio::fs::create_dir_all(&temporary_root)
        .await
        .map_err(|_| VoiceDownloadError::Storage)?;
    let temporary = temporary_root.join(format!(
        "voice-download-{}-{}.tmp",
        std::process::id(),
        DOWNLOAD_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut staged = StagedVoice {
        path: temporary,
        bytes: 0,
    };
    let client = reqwest::Client::builder()
        .tls_backend_rustls()
        .redirect(reqwest::redirect::Policy::limited(10))
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| VoiceDownloadError::Network)?;
    let operation = async {
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|_| VoiceDownloadError::Network)?;
        if !response.status().is_success() {
            return Err(VoiceDownloadError::Status);
        }
        let declared = response.content_length();
        if declared.is_some_and(|bytes| bytes > MAX_RIFF_REFERENCE_BYTES) {
            return Err(VoiceDownloadError::TooLarge);
        }
        if expected.is_some_and(|value| declared.is_some_and(|bytes| bytes != value.bytes)) {
            return Err(VoiceDownloadError::Integrity);
        }
        let mut warned = false;
        if let Some(bytes) = declared.filter(|bytes| *bytes > LARGE_REFERENCE_WARNING_BYTES) {
            large_warning(bytes);
            warned = true;
        }

        let standard = open_private_staging(staged.path())?;
        let mut file = tokio::fs::File::from_std(standard);
        let mut stream = response.bytes_stream();
        let mut total = 0_u64;
        let mut digest = Sha256::new();
        while let Some(next) = tokio::select! {
            () = cancellation.cancelled() => return Err(VoiceDownloadError::Cancelled),
            next = stream.next() => next,
        } {
            let chunk = next.map_err(|_| VoiceDownloadError::Network)?;
            total = total
                .checked_add(chunk.len() as u64)
                .ok_or(VoiceDownloadError::TooLarge)?;
            if total > MAX_RIFF_REFERENCE_BYTES {
                return Err(VoiceDownloadError::TooLarge);
            }
            if expected.is_some_and(|value| total > value.bytes) {
                return Err(VoiceDownloadError::Integrity);
            }
            if !warned && total > LARGE_REFERENCE_WARNING_BYTES {
                large_warning(total);
                warned = true;
            }
            digest.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|_| VoiceDownloadError::Storage)?;
        }
        file.sync_all()
            .await
            .map_err(|_| VoiceDownloadError::Storage)?;
        if let Some(expected) = expected
            && (total != expected.bytes || format!("{:x}", digest.finalize()) != expected.sha256)
        {
            return Err(VoiceDownloadError::Integrity);
        }
        Ok(total)
    };
    let bytes = tokio::select! {
        () = cancellation.cancelled() => Err(VoiceDownloadError::Cancelled),
        result = tokio::time::timeout(Duration::from_secs(600), operation) => {
            result.map_err(|_| VoiceDownloadError::Timeout)?
        }
    }?;
    staged.bytes = bytes;
    Ok(staged)
}

fn open_private_staging(path: &Path) -> Result<std::fs::File, VoiceDownloadError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|_| VoiceDownloadError::Storage)
}

#[must_use]
pub fn curated_download_url(voice: &CuratedVoice) -> Url {
    Url::parse(&format!(
        "https://huggingface.co/{}/resolve/{}/{}?download=true",
        voice.repository, voice.revision, voice.path
    ))
    .expect("curated voice URL is static and valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_classification_preserves_paths_and_requires_explicit_http() {
        assert!(matches!(
            VoiceSource::parse(OsString::from("voice.wav")).unwrap(),
            VoiceSource::File(_)
        ));
        assert!(matches!(
            VoiceSource::parse(OsString::from(r"C:\voices\sample.wav")).unwrap(),
            VoiceSource::File(_)
        ));
        assert!(matches!(
            VoiceSource::parse(OsString::from("https://private.test/voice.wav")).unwrap(),
            VoiceSource::Url(_)
        ));
        assert!(matches!(
            VoiceSource::parse(OsString::from("HTTP://private.test/voice.wav")).unwrap(),
            VoiceSource::Url(_)
        ));
        assert!(VoiceSource::parse(OsString::from("https://[invalid")).is_err());
    }

    #[test]
    fn curated_manifest_is_unique_pinned_and_complete() {
        let mut ids: Vec<_> = CURATED_VOICES.iter().map(|voice| voice.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), CURATED_VOICES.len());
        for voice in CURATED_VOICES {
            assert_eq!(voice.revision.len(), 40);
            assert_eq!(voice.sha256.len(), 64);
            assert!(voice.bytes > 44);
            assert_eq!(curated_download_url(voice).scheme(), "https");
        }
    }
}
