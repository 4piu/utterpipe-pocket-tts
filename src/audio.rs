use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use thiserror::Error;

/// Inputs larger than this are unusual for the supported prompt format. This is
/// a diagnostic threshold, not an import limit.
pub const LARGE_REFERENCE_WARNING_BYTES: u64 = 5 * 1_024 * 1_024;
/// Largest file representable by classic RIFF's 32-bit size field.
pub const MAX_RIFF_REFERENCE_BYTES: u64 = u32::MAX as u64 + 8;

#[derive(Clone)]
pub struct ReferenceAudio {
    pub samples: Vec<i16>,
    pub sample_rate: u32,
    pub source_sha256: String,
    pub samples_sha256: String,
}

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("reference audio could not be opened safely")]
    Open,
    #[error("reference audio must be a regular file")]
    FilePolicy,
    #[error("reference audio is not a supported mono PCM16 RIFF/WAVE file")]
    InvalidWav,
    #[error("reference audio duration is outside the allowed range")]
    Duration,
    #[error("reference audio import was cancelled")]
    Cancelled,
    #[error("generated audio contains a non-finite sample")]
    NonFinite,
}

/// Read and strictly validate a user-approved voice reference.
///
/// # Errors
///
/// Returns [`AudioError`] for unsafe file types, I/O failures, malformed WAV,
/// unsupported rate/channel/format, or a duration outside the requested bound.
pub fn read_reference(path: &Path, maximum_seconds: f64) -> Result<ReferenceAudio, AudioError> {
    read_reference_cancelled(path, maximum_seconds, || false)
}

/// Read a reference while checking `cancelled` between bounded I/O chunks.
///
/// # Errors
///
/// Returns [`AudioError`] for cancellation, unsafe file types, I/O failures,
/// malformed WAV, unsupported rate/channel/format, or invalid duration.
pub fn read_reference_cancelled(
    path: &Path,
    maximum_seconds: f64,
    cancelled: impl Fn() -> bool,
) -> Result<ReferenceAudio, AudioError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(path).map_err(|_| AudioError::Open)?;
    let metadata = file.metadata().map_err(|_| AudioError::Open)?;
    if !metadata.is_file() {
        return Err(AudioError::FilePolicy);
    }
    let parsed = inspect_pcm16_wav(&mut file, metadata.len(), &cancelled)?;
    if parsed.channels != 1 || !(16_000..=48_000).contains(&parsed.sample_rate) {
        return Err(AudioError::InvalidWav);
    }
    let sample_count = parsed.data_bytes / 2;
    let seconds = sample_count as f64 / f64::from(parsed.sample_rate);
    if !(1.0..=maximum_seconds.min(30.0)).contains(&seconds) {
        return Err(AudioError::Duration);
    }
    let sample_capacity = usize::try_from(sample_count).map_err(|_| AudioError::InvalidWav)?;
    let mut samples = Vec::new();
    samples
        .try_reserve_exact(sample_capacity)
        .map_err(|_| AudioError::InvalidWav)?;
    file.seek(SeekFrom::Start(parsed.data_offset))
        .map_err(|_| AudioError::Open)?;
    let mut remaining = parsed.data_bytes;
    let mut sample_digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1_024];
    while remaining != 0 {
        check_cancelled(&cancelled)?;
        let wanted = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| AudioError::InvalidWav)?;
        file.read_exact(&mut buffer[..wanted])
            .map_err(|_| AudioError::Open)?;
        sample_digest.update(&buffer[..wanted]);
        for sample in buffer[..wanted].chunks_exact(2) {
            samples.push(i16::from_le_bytes([sample[0], sample[1]]));
        }
        remaining -= wanted as u64;
    }
    let samples_sha256 = format!("{:x}", sample_digest.finalize());
    if samples_sha256 != parsed.data_sha256 {
        return Err(AudioError::Open);
    }
    Ok(ReferenceAudio {
        samples,
        sample_rate: parsed.sample_rate,
        source_sha256: parsed.source_sha256,
        samples_sha256,
    })
}

struct InspectedWav {
    sample_rate: u32,
    channels: u16,
    data_offset: u64,
    data_bytes: u64,
    source_sha256: String,
    data_sha256: String,
}

fn inspect_pcm16_wav(
    file: &mut File,
    file_bytes: u64,
    cancelled: &impl Fn() -> bool,
) -> Result<InspectedWav, AudioError> {
    let mut source_digest = Sha256::new();
    let mut header = [0_u8; 12];
    read_exact_hashed(file, &mut header, &mut source_digest, cancelled)?;
    if &header[..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err(AudioError::InvalidWav);
    }
    if u64::from(read_u32(&header, 4)?) + 8 != file_bytes {
        return Err(AudioError::InvalidWav);
    }
    let mut position = 12_u64;
    let mut format = None;
    let mut data = None;
    let mut data_sha256 = None;
    while position < file_bytes {
        if position.checked_add(8).is_none_or(|end| end > file_bytes) {
            return Err(AudioError::InvalidWav);
        }
        let mut chunk_header = [0_u8; 8];
        read_exact_hashed(file, &mut chunk_header, &mut source_digest, cancelled)?;
        let id = &chunk_header[..4];
        let size = u64::from(read_u32(&chunk_header, 4)?);
        let start = position + 8;
        let end = start.checked_add(size).ok_or(AudioError::InvalidWav)?;
        let padded_end = end.checked_add(size & 1).ok_or(AudioError::InvalidWav)?;
        if padded_end > file_bytes {
            return Err(AudioError::InvalidWav);
        }
        if id == b"fmt " {
            if format.is_some() || size < 16 {
                return Err(AudioError::InvalidWav);
            }
            let mut format_bytes = [0_u8; 16];
            read_exact_hashed(file, &mut format_bytes, &mut source_digest, cancelled)?;
            let encoding = read_u16(&format_bytes, 0)?;
            let channels = read_u16(&format_bytes, 2)?;
            let rate = read_u32(&format_bytes, 4)?;
            let byte_rate = read_u32(&format_bytes, 8)?;
            let align = read_u16(&format_bytes, 12)?;
            let bits = read_u16(&format_bytes, 14)?;
            let expected_align = channels.checked_mul(2).ok_or(AudioError::InvalidWav)?;
            let expected_rate = rate
                .checked_mul(u32::from(expected_align))
                .ok_or(AudioError::InvalidWav)?;
            if encoding != 1
                || bits != 16
                || channels == 0
                || align != expected_align
                || byte_rate != expected_rate
            {
                return Err(AudioError::InvalidWav);
            }
            format = Some((channels, rate, usize::from(align)));
            consume_hashed(file, size - 16, &mut source_digest, None, cancelled)?;
        } else if id == b"data" {
            if data.replace((start, size)).is_some() {
                return Err(AudioError::InvalidWav);
            }
            let mut digest = Sha256::new();
            consume_hashed(file, size, &mut source_digest, Some(&mut digest), cancelled)?;
            data_sha256 = Some(format!("{:x}", digest.finalize()));
        } else {
            consume_hashed(file, size, &mut source_digest, None, cancelled)?;
        }
        if size & 1 != 0 {
            let mut padding = [0_u8; 1];
            read_exact_hashed(file, &mut padding, &mut source_digest, cancelled)?;
        }
        position = padded_end;
    }
    if position != file_bytes {
        return Err(AudioError::InvalidWav);
    }
    let (channels, sample_rate, align) = format.ok_or(AudioError::InvalidWav)?;
    let (data_offset, data_bytes) = data.ok_or(AudioError::InvalidWav)?;
    if data_bytes == 0 || data_bytes % align as u64 != 0 {
        return Err(AudioError::InvalidWav);
    }
    Ok(InspectedWav {
        sample_rate,
        channels,
        data_offset,
        data_bytes,
        source_sha256: format!("{:x}", source_digest.finalize()),
        data_sha256: data_sha256.ok_or(AudioError::InvalidWav)?,
    })
}

fn read_exact_hashed(
    file: &mut File,
    bytes: &mut [u8],
    source_digest: &mut Sha256,
    cancelled: &impl Fn() -> bool,
) -> Result<(), AudioError> {
    check_cancelled(cancelled)?;
    file.read_exact(bytes).map_err(|_| AudioError::Open)?;
    source_digest.update(bytes);
    Ok(())
}

fn consume_hashed(
    file: &mut File,
    mut remaining: u64,
    source_digest: &mut Sha256,
    mut chunk_digest: Option<&mut Sha256>,
    cancelled: &impl Fn() -> bool,
) -> Result<(), AudioError> {
    let mut buffer = [0_u8; 64 * 1_024];
    while remaining != 0 {
        check_cancelled(cancelled)?;
        let wanted = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| AudioError::InvalidWav)?;
        file.read_exact(&mut buffer[..wanted])
            .map_err(|_| AudioError::Open)?;
        source_digest.update(&buffer[..wanted]);
        if let Some(digest) = chunk_digest.as_deref_mut() {
            digest.update(&buffer[..wanted]);
        }
        remaining -= wanted as u64;
    }
    Ok(())
}

fn check_cancelled(cancelled: &impl Fn() -> bool) -> Result<(), AudioError> {
    if cancelled() {
        Err(AudioError::Cancelled)
    } else {
        Ok(())
    }
}

/// Write normalized mono PCM16 audio into a new file.
///
/// # Errors
///
/// Returns an I/O error if the destination cannot be created, written, or synced.
pub fn write_pcm16_wav(path: &Path, sample_rate: u32, samples: &[i16]) -> std::io::Result<()> {
    let payload_bytes = u32::try_from(samples.len().saturating_mul(2))
        .map_err(|_| std::io::Error::other("reference audio is too large"))?;
    let mut file = File::create(path)?;
    file.write_all(b"RIFF")?;
    file.write_all(&(36_u32 + payload_bytes).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16_u32.to_le_bytes())?;
    file.write_all(&1_u16.to_le_bytes())?;
    file.write_all(&1_u16.to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&(sample_rate * 2).to_le_bytes())?;
    file.write_all(&2_u16.to_le_bytes())?;
    file.write_all(&16_u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&payload_bytes.to_le_bytes())?;
    for sample in samples {
        file.write_all(&sample.to_le_bytes())?;
    }
    file.sync_all()
}

#[must_use]
pub fn pcm16_wav_bytes(sample_rate: u32, samples: &[u8]) -> Option<Vec<u8>> {
    let payload_bytes = u32::try_from(samples.len()).ok()?;
    if payload_bytes == 0 || payload_bytes % 2 != 0 {
        return None;
    }
    let mut wav = Vec::with_capacity(samples.len() + 44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36_u32.checked_add(payload_bytes)?).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate.checked_mul(2)?).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&payload_bytes.to_le_bytes());
    wav.extend_from_slice(samples);
    Some(wav)
}

/// Convert finite float samples to signed little-endian PCM16.
///
/// # Errors
///
/// Returns [`AudioError::NonFinite`] if any engine sample is NaN or infinite.
pub fn floats_to_pcm16(samples: &[f32]) -> Result<Vec<u8>, AudioError> {
    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for &sample in samples {
        let value = float_to_pcm16(sample)?;
        pcm.extend_from_slice(&value.to_le_bytes());
    }
    Ok(pcm)
}

/// Hash float samples exactly as their converted signed PCM16 byte stream.
///
/// # Errors
///
/// Returns [`AudioError::NonFinite`] if any sample is NaN or infinite.
pub fn floats_pcm16_sha256(samples: &[f32]) -> Result<[u8; 32], AudioError> {
    let mut digest = Sha256::new();
    for &sample in samples {
        digest.update(float_to_pcm16(sample)?.to_le_bytes());
    }
    Ok(digest.finalize().into())
}

fn float_to_pcm16(sample: f32) -> Result<i16, AudioError> {
    if !sample.is_finite() {
        return Err(AudioError::NonFinite);
    }
    let clamped = sample.clamp(-1.0, 1.0);
    Ok(if clamped <= -1.0 {
        i16::MIN
    } else if clamped >= 1.0 {
        i16::MAX
    } else {
        (clamped * 32_767.0).round() as i16
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, AudioError> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or(AudioError::InvalidWav)?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, AudioError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or(AudioError::InvalidWav)?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, fs::File, io::Write};

    use tempfile::TempDir;

    use super::*;

    fn write_reference(path: &Path, junk_bytes: u32) {
        let sample_count = 16_000_u32;
        let data_bytes = sample_count * 2;
        let riff_bytes = 36_u32 + data_bytes + 8 + junk_bytes + (junk_bytes & 1);
        let mut file = File::create(path).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&riff_bytes.to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16_u32.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&16_000_u32.to_le_bytes()).unwrap();
        file.write_all(&32_000_u32.to_le_bytes()).unwrap();
        file.write_all(&2_u16.to_le_bytes()).unwrap();
        file.write_all(&16_u16.to_le_bytes()).unwrap();
        file.write_all(b"JUNK").unwrap();
        file.write_all(&junk_bytes.to_le_bytes()).unwrap();
        let zeroes = [0_u8; 64 * 1_024];
        let mut remaining = u64::from(junk_bytes);
        while remaining != 0 {
            let count = usize::try_from(remaining.min(zeroes.len() as u64)).unwrap();
            file.write_all(&zeroes[..count]).unwrap();
            remaining -= count as u64;
        }
        if junk_bytes & 1 != 0 {
            file.write_all(&[0]).unwrap();
        }
        file.write_all(b"data").unwrap();
        file.write_all(&data_bytes.to_le_bytes()).unwrap();
        for _ in 0..sample_count {
            file.write_all(&200_i16.to_le_bytes()).unwrap();
        }
    }

    #[test]
    fn float_conversion_clips_exactly() {
        let pcm = floats_to_pcm16(&[-2.0, -1.0, 0.0, 1.0, 2.0]).unwrap();
        let values: Vec<_> = pcm
            .chunks_exact(2)
            .map(|v| i16::from_le_bytes([v[0], v[1]]))
            .collect();
        assert_eq!(values, [i16::MIN, i16::MIN, 0, i16::MAX, i16::MAX]);
        assert!(floats_to_pcm16(&[f32::NAN]).is_err());
    }

    #[test]
    fn metadata_larger_than_warning_threshold_is_streamed_not_rejected() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("large-metadata.wav");
        write_reference(
            &path,
            u32::try_from(LARGE_REFERENCE_WARNING_BYTES + 1).unwrap(),
        );

        let reference = read_reference(&path, 30.0).unwrap();

        assert_eq!(reference.sample_rate, 16_000);
        assert_eq!(reference.samples.len(), 16_000);
    }

    #[test]
    fn streaming_metadata_parse_observes_cancellation() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("cancel.wav");
        write_reference(&path, 1_048_576);
        let checks = Cell::new(0_u32);

        let error = read_reference_cancelled(&path, 30.0, || {
            checks.set(checks.get() + 1);
            checks.get() > 4
        })
        .err()
        .unwrap();

        assert!(matches!(error, AudioError::Cancelled));
    }
}
