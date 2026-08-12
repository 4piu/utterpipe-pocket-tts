//! Provider-neutral synthesis types shared by the active XN engine adapter.

use std::time::Duration;

use thiserror::Error;

pub const SAMPLE_RATE: u32 = 24_000;

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
    pub seed: u32,
    pub timeout: Duration,
    pub max_audio_bytes: usize,
}
