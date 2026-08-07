use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsModelConfig,
    OfflineTtsPocketModelConfig,
};
use thiserror::Error;

use crate::{audio, audio::ReferenceAudio};

pub const SAMPLE_RATE: u32 = 24_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngineOptions {
    pub num_threads: u32,
    pub speed: f64,
    pub seed: u32,
    pub max_reference_audio_seconds: f64,
    pub voice_embedding_cache_capacity: u32,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            num_threads: 2,
            speed: 1.0,
            seed: 42,
            max_reference_audio_seconds: 10.0,
            voice_embedding_cache_capacity: 16,
        }
    }
}

impl EngineOptions {
    /// Validate every engine-specific startup option.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::InvalidOptions`] for an out-of-range value.
    pub fn validate(&self) -> Result<(), EngineError> {
        if !(1..=64).contains(&self.num_threads)
            || !self.speed.is_finite()
            || !(0.5..=2.0).contains(&self.speed)
            || !self.max_reference_audio_seconds.is_finite()
            || !(1.0..=30.0).contains(&self.max_reference_audio_seconds)
            || !(1..=128).contains(&self.voice_embedding_cache_capacity)
        {
            return Err(EngineError::InvalidOptions);
        }
        Ok(())
    }
}

#[must_use]
#[derive(Debug, Error, Clone, Copy, Eq, PartialEq)]
pub enum EngineError {
    #[error("provider options are invalid")]
    InvalidOptions,
    #[error("Pocket TTS engine is unavailable")]
    Unavailable,
    #[error("Pocket TTS generation failed")]
    Failed,
    #[error("Pocket TTS generation was cancelled")]
    Cancelled,
    #[error("Pocket TTS generation exceeded its deadline")]
    Timeout,
    #[error("Pocket TTS output exceeded its byte limit")]
    OutputTooLarge,
}

#[derive(Clone, Copy, Debug)]
pub struct GenerationSummary {
    pub byte_length: usize,
    pub frame_count: usize,
    pub pcm_sha256: [u8; 32],
}

#[derive(Clone, Copy, Debug)]
pub struct GenerationOptions {
    pub speed: f64,
    pub seed: u32,
    pub timeout: Duration,
    pub max_audio_bytes: usize,
}

pub struct PocketEngine {
    tts: OfflineTts,
    options: EngineOptions,
}

impl PocketEngine {
    /// Construct the pinned Pocket engine exclusively from explicit local paths.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Unavailable`] if the native engine rejects the model.
    pub fn create(model: &Path, options: EngineOptions) -> Result<Self, EngineError> {
        options.validate()?;
        let pocket = OfflineTtsPocketModelConfig {
            lm_flow: Some(model_file(model, "lm_flow.int8.onnx")?),
            lm_main: Some(model_file(model, "lm_main.int8.onnx")?),
            encoder: Some(model_file(model, "encoder.onnx")?),
            decoder: Some(model_file(model, "decoder.int8.onnx")?),
            text_conditioner: Some(model_file(model, "text_conditioner.onnx")?),
            vocab_json: Some(model_file(model, "vocab.json")?),
            token_scores_json: Some(model_file(model, "token_scores.json")?),
            voice_embedding_cache_capacity: i32::try_from(options.voice_embedding_cache_capacity)
                .map_err(|_| EngineError::InvalidOptions)?,
        };
        let config = OfflineTtsConfig {
            model: OfflineTtsModelConfig {
                pocket,
                num_threads: i32::try_from(options.num_threads)
                    .map_err(|_| EngineError::InvalidOptions)?,
                debug: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let tts = OfflineTts::create(&config).ok_or(EngineError::Unavailable)?;
        if tts.sample_rate() != SAMPLE_RATE as i32 {
            return Err(EngineError::Unavailable);
        }
        Ok(Self { tts, options })
    }

    /// Generate genuine callback-delivered PCM chunks into a bounded channel.
    ///
    /// # Errors
    ///
    /// Returns a stable engine error for cancellation, timeout, output bounds,
    /// invalid samples, a closed consumer, or native generation failure.
    pub fn generate(
        &self,
        text: &str,
        reference: &ReferenceAudio,
        options: GenerationOptions,
        cancellation: Arc<AtomicBool>,
        sender: tokio::sync::mpsc::Sender<Vec<u8>>,
    ) -> Result<GenerationSummary, EngineError> {
        if !options.speed.is_finite() || !(0.5..=2.0).contains(&options.speed) {
            return Err(EngineError::InvalidOptions);
        }
        let sample_limit = (f64::from(reference.sample_rate)
            * self.options.max_reference_audio_seconds)
            .floor() as usize;
        let reference_samples: Vec<f32> = reference
            .samples
            .iter()
            .take(sample_limit)
            .map(|sample| f32::from(*sample) / 32_768.0)
            .collect();
        let mut extra = HashMap::new();
        extra.insert(
            "max_reference_audio_len".to_owned(),
            json!(self.options.max_reference_audio_seconds),
        );
        extra.insert("seed".to_owned(), json!(options.seed));
        let config = GenerationConfig {
            // Silence post-scaling is a whole-output transform performed after
            // callbacks. Disabling it keeps callback concatenation identical to
            // the final native audio and makes incremental delivery truthful.
            silence_scale: 1.0,
            speed: options.speed as f32,
            reference_audio: Some(reference_samples),
            reference_sample_rate: i32::try_from(reference.sample_rate)
                .map_err(|_| EngineError::Failed)?,
            extra: Some(extra),
            ..Default::default()
        };
        let status = Arc::new(Mutex::new(None));
        let callback_status = Arc::clone(&status);
        // Pocket reports newly generated consecutive chunks (the generic sherpa
        // wrapper's cumulative-callback wording does not match this backend).
        let counters = Arc::new(Mutex::new((0_usize, 0_usize, 0_usize)));
        let callback_counters = Arc::clone(&counters);
        let digest = Arc::new(Mutex::new(Sha256::new()));
        let callback_digest = Arc::clone(&digest);
        let callback_cancel = Arc::clone(&cancellation);
        let started = Instant::now();
        let audio = self.tts.generate_with_config(
            text,
            &config,
            Some(move |samples: &[f32], _progress: f32| {
                let stop = if callback_cancel.load(Ordering::Acquire) {
                    Some(EngineError::Cancelled)
                } else if started.elapsed() >= options.timeout {
                    Some(EngineError::Timeout)
                } else {
                    None
                };
                if let Some(error) = stop {
                    set_status(&callback_status, error);
                    return false;
                }
                let Ok(mut counters) = callback_counters.lock() else {
                    set_status(&callback_status, EngineError::Failed);
                    return false;
                };
                let Some(new_byte_length) = samples.len().checked_mul(2) else {
                    set_status(&callback_status, EngineError::OutputTooLarge);
                    return false;
                };
                if new_byte_length > options.max_audio_bytes.saturating_sub(counters.0) {
                    set_status(&callback_status, EngineError::OutputTooLarge);
                    return false;
                }
                let pcm = match audio::floats_to_pcm16(samples) {
                    Ok(pcm) if !pcm.is_empty() => pcm,
                    Ok(_) => return true,
                    Err(_) => {
                        set_status(&callback_status, EngineError::Failed);
                        return false;
                    }
                };
                debug_assert_eq!(pcm.len(), new_byte_length);
                counters.0 += pcm.len();
                let Some(sample_count) = counters.2.checked_add(samples.len()) else {
                    set_status(&callback_status, EngineError::OutputTooLarge);
                    return false;
                };
                counters.2 = sample_count;
                let Ok(mut digest) = callback_digest.lock() else {
                    set_status(&callback_status, EngineError::Failed);
                    return false;
                };
                digest.update(&pcm);
                drop(digest);
                drop(counters);
                for frame in pcm.chunks(1_048_576) {
                    if let Ok(mut counters) = callback_counters.lock() {
                        counters.1 += 1;
                    } else {
                        set_status(&callback_status, EngineError::Failed);
                        return false;
                    }
                    if sender.blocking_send(frame.to_vec()).is_err() {
                        set_status(&callback_status, EngineError::Cancelled);
                        return false;
                    }
                }
                true
            }),
        );
        if let Some(error) = status.lock().ok().and_then(|guard| *guard) {
            return Err(error);
        }
        let audio = audio.ok_or(EngineError::Failed)?;
        if cancellation.load(Ordering::Acquire) {
            return Err(EngineError::Cancelled);
        }
        if audio.sample_rate() != SAMPLE_RATE as i32 {
            return Err(EngineError::Failed);
        }
        let (byte_length, frame_count, sample_count) =
            *counters.lock().map_err(|_| EngineError::Failed)?;
        let pcm_sha256: [u8; 32] = digest
            .lock()
            .map_err(|_| EngineError::Failed)?
            .clone()
            .finalize()
            .into();
        let final_pcm_sha256 =
            audio::floats_pcm16_sha256(audio.samples()).map_err(|_| EngineError::Failed)?;
        if byte_length == 0
            || frame_count == 0
            || audio.samples().len() != sample_count
            || sample_count.checked_mul(2) != Some(byte_length)
            || pcm_sha256 != final_pcm_sha256
        {
            return Err(EngineError::Failed);
        }
        Ok(GenerationSummary {
            byte_length,
            frame_count,
            pcm_sha256,
        })
    }
}

fn model_file(model: &Path, name: &str) -> Result<String, EngineError> {
    model
        .join(name)
        .into_os_string()
        .into_string()
        .map_err(|_| EngineError::Unavailable)
}

fn set_status(status: &Mutex<Option<EngineError>>, error: EngineError) {
    if let Ok(mut status) = status.lock() {
        *status = Some(error);
    }
}

#[cfg(test)]
pub(crate) fn mock_generate(
    chunks: &[Vec<f32>],
    sender: tokio::sync::mpsc::Sender<Vec<u8>>,
    cancellation: &AtomicBool,
    max_audio_bytes: usize,
) -> Result<GenerationSummary, EngineError> {
    let mut byte_length = 0_usize;
    let mut frame_count = 0_usize;
    let mut digest = Sha256::new();
    for chunk in chunks {
        if cancellation.load(Ordering::Acquire) {
            return Err(EngineError::Cancelled);
        }
        let chunk_bytes = chunk
            .len()
            .checked_mul(2)
            .ok_or(EngineError::OutputTooLarge)?;
        if chunk_bytes > max_audio_bytes.saturating_sub(byte_length) {
            return Err(EngineError::OutputTooLarge);
        }
        let pcm = audio::floats_to_pcm16(chunk).map_err(|_| EngineError::Failed)?;
        byte_length += pcm.len();
        frame_count += 1;
        digest.update(&pcm);
        sender
            .blocking_send(pcm)
            .map_err(|_| EngineError::Cancelled)?;
    }
    Ok(GenerationSummary {
        byte_length,
        frame_count,
        pcm_sha256: digest.finalize().into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn mock_callbacks_are_consecutive_and_bounded() {
        let chunks = vec![vec![0.0, 0.5], vec![-0.5, 1.0]];
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        let chunks_copy = chunks.clone();
        let worker = std::thread::spawn(move || {
            mock_generate(&chunks_copy, sender, &AtomicBool::new(false), 16)
        });
        let mut output = Vec::new();
        while let Some(chunk) = receiver.recv().await {
            output.extend(chunk);
        }
        let result = worker.join().unwrap().unwrap();
        assert_eq!(result.byte_length, 8);
        assert_eq!(result.frame_count, 2);
        assert_eq!(output.len(), 8);
        let expected_digest: [u8; 32] = Sha256::digest(&output).into();
        assert_eq!(result.pcm_sha256, expected_digest);
    }

    #[test]
    fn mock_rejects_size_before_pcm_allocation_or_conversion() {
        let (sender, _receiver) = tokio::sync::mpsc::channel(1);
        let result = mock_generate(&[vec![f32::NAN, 0.0]], sender, &AtomicBool::new(false), 3);
        assert!(matches!(result, Err(EngineError::OutputTooLarge)));
    }
}
