use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use thiserror::Error;

pub const MAX_REFERENCE_BYTES: u64 = 5 * 1_024 * 1_024;

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
    #[error("reference audio must be a regular file no larger than 5 MiB")]
    FilePolicy,
    #[error("reference audio is not a supported mono PCM16 RIFF/WAVE file")]
    InvalidWav,
    #[error("reference audio duration is outside the allowed range")]
    Duration,
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
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options.open(path).map_err(|_| AudioError::Open)?;
    let metadata = file.metadata().map_err(|_| AudioError::Open)?;
    if !metadata.is_file() || metadata.len() > MAX_REFERENCE_BYTES {
        return Err(AudioError::FilePolicy);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(MAX_REFERENCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AudioError::Open)?;
    if bytes.len() as u64 > MAX_REFERENCE_BYTES {
        return Err(AudioError::FilePolicy);
    }
    if bytes.len() as u64 != metadata.len() {
        return Err(AudioError::Open);
    }
    let source_sha256 = hex_digest(&bytes);
    let parsed = parse_pcm16_wav(&bytes)?;
    if parsed.channels != 1 || !(16_000..=48_000).contains(&parsed.sample_rate) {
        return Err(AudioError::InvalidWav);
    }
    let seconds = parsed.samples.len() as f64 / f64::from(parsed.sample_rate);
    if !(1.0..=maximum_seconds.min(30.0)).contains(&seconds) {
        return Err(AudioError::Duration);
    }
    let mut decoded_bytes = Vec::with_capacity(parsed.samples.len() * 2);
    for sample in &parsed.samples {
        decoded_bytes.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(ReferenceAudio {
        samples: parsed.samples,
        sample_rate: parsed.sample_rate,
        source_sha256,
        samples_sha256: hex_digest(&decoded_bytes),
    })
}

struct ParsedWav {
    samples: Vec<i16>,
    sample_rate: u32,
    channels: u16,
}

fn parse_pcm16_wav(bytes: &[u8]) -> Result<ParsedWav, AudioError> {
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(AudioError::InvalidWav);
    }
    if read_u32(bytes, 4)? as usize + 8 != bytes.len() {
        return Err(AudioError::InvalidWav);
    }
    let mut position = 12_usize;
    let mut format = None;
    let mut data = None;
    while position < bytes.len() {
        if position + 8 > bytes.len() {
            return Err(AudioError::InvalidWav);
        }
        let id = &bytes[position..position + 4];
        let size = read_u32(bytes, position + 4)? as usize;
        let start = position + 8;
        let end = start.checked_add(size).ok_or(AudioError::InvalidWav)?;
        if end > bytes.len() {
            return Err(AudioError::InvalidWav);
        }
        if id == b"fmt " {
            if format.is_some() || size < 16 {
                return Err(AudioError::InvalidWav);
            }
            let encoding = read_u16(bytes, start)?;
            let channels = read_u16(bytes, start + 2)?;
            let rate = read_u32(bytes, start + 4)?;
            let byte_rate = read_u32(bytes, start + 8)?;
            let align = read_u16(bytes, start + 12)?;
            let bits = read_u16(bytes, start + 14)?;
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
        } else if id == b"data" && data.replace(&bytes[start..end]).is_some() {
            return Err(AudioError::InvalidWav);
        }
        position = end.checked_add(size & 1).ok_or(AudioError::InvalidWav)?;
        if position > bytes.len() {
            return Err(AudioError::InvalidWav);
        }
    }
    if position != bytes.len() {
        return Err(AudioError::InvalidWav);
    }
    let (channels, sample_rate, align) = format.ok_or(AudioError::InvalidWav)?;
    let data = data.ok_or(AudioError::InvalidWav)?;
    if data.is_empty() || data.len() % align != 0 {
        return Err(AudioError::InvalidWav);
    }
    let samples = data
        .chunks_exact(2)
        .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
        .collect();
    Ok(ParsedWav {
        samples,
        sample_rate,
        channels,
    })
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

fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
