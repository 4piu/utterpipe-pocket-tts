//! Bounded adapter for the pinned native XN Pocket TTS runtime.

use std::{
    fs::File,
    io::BufReader,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use ptts::{
    Tokenizer,
    mimi::MimiDecoderState,
    tts_model::{MimiEnc, TTSConfig, TTSModel, TTSState},
};
use rand::SeedableRng;
use rubato::Resampler;
use sha2::{Digest, Sha256};
use xn::{BackendQ, Tensor, Unquantized, nn::VB, quantized::Q80F32};

use crate::{
    audio,
    audio::ReferenceAudio,
    engine::{EngineError, GenerationOptions, GenerationSummary, SAMPLE_RATE},
};

const MAX_CHUNK_TOKENS: usize = 50;
const STATE_SEQUENCE_CAPACITY: usize = 1_024;
const PIPELINE_CAPACITY: usize = 2;
const RESAMPLE_CHUNK_FRAMES: usize = 1_024;

pub struct XnVoiceEncoder {
    encoder: MimiEnc<Unquantized<f32, xn::CpuDevice>>,
    sample_rate: u32,
    maximum_audio_seconds: f32,
    model_extension: String,
}

impl XnVoiceEncoder {
    /// Load the voice encoder from a full XN GGUF bundle.
    ///
    /// # Errors
    ///
    /// Returns a stable error if the bundle is unavailable, incompatible, or
    /// advertises unsafe prompt bounds.
    pub fn create(
        config_path: &Path,
        model_path: &Path,
        num_threads: u32,
    ) -> Result<Self, EngineError> {
        if !(1..=64).contains(&num_threads)
            || model_path.extension().and_then(|value| value.to_str()) != Some("gguf")
        {
            return Err(EngineError::InvalidOptions);
        }
        if !config_path.is_file() || !model_path.is_file() {
            return Err(EngineError::Unavailable);
        }
        xn::set_num_threads(num_threads as usize);
        let config: TTSConfig = serde_json::from_reader(BufReader::new(
            File::open(config_path).map_err(|_| EngineError::Unavailable)?,
        ))
        .map_err(|_| EngineError::Unavailable)?;
        let sample_rate =
            u32::try_from(config.mimi.sample_rate).map_err(|_| EngineError::Unavailable)?;
        if sample_rate != SAMPLE_RATE
            || !config.audio_prompt_max_duration.is_finite()
            || !(1.0..=30.0).contains(&config.audio_prompt_max_duration)
        {
            return Err(EngineError::Unavailable);
        }
        // The official April config leaves `model_id` unset. Match the
        // upstream voice-preparation tool's informational fallback; the
        // provider bundle manifest remains the authoritative compatibility
        // token.
        let model_extension = config.model_ext().unwrap_or_else(|| "unknown".to_owned());
        let reader = BufReader::new(File::open(model_path).map_err(|_| EngineError::Unavailable)?);
        let weights = VB::load_gguf_with_key_map(reader, xn::CPU, remap_key)
            .map_err(|_| EngineError::Unavailable)?;
        let encoder = MimiEnc::<Unquantized<f32, xn::CpuDevice>>::load(&weights.root(), &config)
            .map_err(|_| EngineError::Unavailable)?;
        Ok(Self {
            encoder,
            sample_rate,
            maximum_audio_seconds: config.audio_prompt_max_duration,
            model_extension,
        })
    }

    /// Prepare a compact voice state from already validated mono PCM16 audio.
    ///
    /// # Errors
    ///
    /// Returns a stable cancellation, I/O, or inference error. Callers should
    /// write into private staging and publish the state atomically.
    pub fn prepare_voice<F>(
        &self,
        reference: &ReferenceAudio,
        output_path: &Path,
        cancelled: F,
    ) -> Result<(), EngineError>
    where
        F: Fn() -> bool,
    {
        check_cancelled(&cancelled)?;
        let minimum_samples =
            usize::try_from(reference.sample_rate).map_err(|_| EngineError::InvalidOptions)?;
        let maximum_samples = minimum_samples
            .checked_mul(30)
            .ok_or(EngineError::InvalidOptions)?;
        if !(16_000..=48_000).contains(&reference.sample_rate)
            || !(minimum_samples..=maximum_samples).contains(&reference.samples.len())
        {
            return Err(EngineError::InvalidOptions);
        }
        let mut samples: Vec<f32> = reference
            .samples
            .iter()
            .map(|sample| f32::from(*sample) / 32_768.0)
            .collect();
        ptts::utils::normalize_loudness(&mut samples, reference.sample_rate)
            .map_err(|_| EngineError::Failed)?;
        check_cancelled(&cancelled)?;
        if reference.sample_rate != self.sample_rate {
            samples = resample(
                &samples,
                reference.sample_rate,
                self.sample_rate,
                &cancelled,
            )?;
        }
        let sample_limit =
            (f64::from(self.sample_rate) * f64::from(self.maximum_audio_seconds)).floor() as usize;
        samples.truncate(sample_limit);
        if samples.is_empty() {
            return Err(EngineError::Failed);
        }
        check_cancelled(&cancelled)?;
        let audio = Tensor::from_vec(samples, (1, 1, ()), &xn::CPU)
            .and_then(|value| value.to::<f32>())
            .map_err(|_| EngineError::Failed)?;
        let voice = self
            .encoder
            .encode_audio(&audio)
            .map_err(|_| EngineError::Failed)?;
        check_cancelled(&cancelled)?;
        let tensors =
            std::collections::HashMap::from([("emb".to_owned(), xn::TypedTensor::F32(voice))]);
        let metadata = std::collections::HashMap::from([(
            "model_ext".to_owned(),
            self.model_extension.clone(),
        )]);
        xn::safetensors::save_with_data_info(&tensors, Some(metadata), output_path)
            .map_err(|_| EngineError::Failed)?;
        check_cancelled(&cancelled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XnModelBehavior {
    pub temperature: f32,
    pub output_gain: f32,
    pub pad_with_spaces_for_short_inputs: bool,
    pub remove_semicolons: bool,
    pub frames_after_eos_offset: usize,
}

impl XnModelBehavior {
    #[must_use]
    pub const fn april_2026_english() -> Self {
        Self {
            temperature: 0.3,
            output_gain: 0.65,
            pad_with_spaces_for_short_inputs: false,
            remove_semicolons: false,
            frames_after_eos_offset: 2,
        }
    }

    fn validate(self) -> Result<Self, EngineError> {
        if !self.temperature.is_finite()
            || self.temperature <= 0.0
            || !self.output_gain.is_finite()
            || !(0.0..=1.0).contains(&self.output_gain)
            || self.output_gain == 0.0
            || self.frames_after_eos_offset > 64
        {
            return Err(EngineError::InvalidOptions);
        }
        Ok(self)
    }
}

struct HfTokenizer(tokenizers::Tokenizer);

impl Tokenizer for HfTokenizer {
    fn encode(&self, text: &str) -> xn::Result<Vec<u32>> {
        Ok(self
            .0
            .encode(text, false)
            .map_err(xn::Error::wrap)?
            .get_ids()
            .to_vec())
    }

    fn decode(&self, tokens: &[u32]) -> xn::Result<String> {
        self.0.decode(tokens, true).map_err(xn::Error::wrap)
    }
}

struct SpTokenizer(sentencepiece::SentencePieceProcessor);

impl Tokenizer for SpTokenizer {
    fn encode(&self, text: &str) -> xn::Result<Vec<u32>> {
        Ok(self
            .0
            .encode(text)
            .map_err(xn::Error::wrap)?
            .into_iter()
            .map(|piece| piece.id)
            .collect())
    }

    fn decode(&self, tokens: &[u32]) -> xn::Result<String> {
        self.0.decode_piece_ids(tokens).map_err(xn::Error::wrap)
    }
}

struct PreparedChunk {
    tokens: Vec<u32>,
    max_frames: usize,
    frames_after_eos: usize,
}

struct ChunkSummary {
    byte_length: usize,
    frame_count: usize,
}

struct NormalRng {
    inner: rand::rngs::StdRng,
    distribution: rand_distr::Normal<f32>,
}

impl NormalRng {
    fn new(temperature: f32, seed: u32) -> Result<Self, EngineError> {
        let distribution = rand_distr::Normal::new(0.0, temperature.sqrt())
            .map_err(|_| EngineError::InvalidOptions)?;
        Ok(Self {
            inner: rand::rngs::StdRng::seed_from_u64(u64::from(seed)),
            distribution,
        })
    }
}

impl ptts::flow_lm::Rng for NormalRng {
    fn sample(&mut self) -> f32 {
        use rand::Rng as _;
        self.inner.sample(self.distribution)
    }
}

pub struct XnPocketEngine {
    model: Arc<TTSModel<Q80F32>>,
    base_tts_state: TTSState<Q80F32>,
    base_mimi_state: MimiDecoderState<f32, xn::CpuDevice>,
    behavior: XnModelBehavior,
    ldim: usize,
}

impl XnPocketEngine {
    /// Load a converted Q8 model and one model-generation-specific voice state.
    ///
    /// # Errors
    ///
    /// Returns a stable unavailable/invalid-options error without forwarding
    /// model paths or third-party parser details.
    pub fn create(
        config_path: &Path,
        model_path: &Path,
        tokenizer_path: &Path,
        voice_state_path: &Path,
        behavior: XnModelBehavior,
        num_threads: u32,
    ) -> Result<Self, EngineError> {
        let behavior = behavior.validate()?;
        if !(1..=64).contains(&num_threads)
            || model_path.extension().and_then(|value| value.to_str()) != Some("gguf")
        {
            return Err(EngineError::InvalidOptions);
        }
        for path in [config_path, model_path, tokenizer_path, voice_state_path] {
            if !path.is_file() {
                return Err(EngineError::Unavailable);
            }
        }

        xn::set_num_threads(num_threads as usize);
        let config: TTSConfig = serde_json::from_reader(BufReader::new(
            File::open(config_path).map_err(|_| EngineError::Unavailable)?,
        ))
        .map_err(|_| EngineError::Unavailable)?;
        if config.mimi.sample_rate != SAMPLE_RATE as usize {
            return Err(EngineError::Unavailable);
        }
        let tokenizer: Box<dyn Tokenizer + Send + Sync> =
            if tokenizer_path.extension().and_then(|value| value.to_str()) == Some("model") {
                Box::new(SpTokenizer(
                    sentencepiece::SentencePieceProcessor::open(tokenizer_path)
                        .map_err(|_| EngineError::Unavailable)?,
                ))
            } else {
                Box::new(HfTokenizer(
                    tokenizers::Tokenizer::from_file(tokenizer_path)
                        .map_err(|_| EngineError::Unavailable)?,
                ))
            };
        let reader = BufReader::new(File::open(model_path).map_err(|_| EngineError::Unavailable)?);
        let weights = VB::load_gguf_with_key_map(reader, xn::CPU, remap_key)
            .map_err(|_| EngineError::Unavailable)?;
        let root = weights.root();
        let model = TTSModel::<Q80F32>::load(&root, tokenizer, &config)
            .map_err(|_| EngineError::Unavailable)?;
        root.check_all_used_with_ignore(ignored_weight)
            .map_err(|_| EngineError::Unavailable)?;

        let voice = load_voice(voice_state_path).map_err(|_| EngineError::Unavailable)?;
        let mut base_tts_state = model
            .init_flow_lm_state(1, STATE_SEQUENCE_CAPACITY)
            .map_err(|_| EngineError::Unavailable)?;
        model
            .prompt_audio(&mut base_tts_state, &voice)
            .map_err(|_| EngineError::Unavailable)?;
        let base_mimi_state = model
            .init_mimi_state(1, 250)
            .map_err(|_| EngineError::Unavailable)?;
        let ldim = config.flow_lm.ldim;

        Ok(Self {
            model: Arc::new(model),
            base_tts_state,
            base_mimi_state,
            behavior,
            ldim,
        })
    }

    /// Generate bounded consecutive PCM16 frames with cooperative cancellation.
    ///
    /// # Errors
    ///
    /// Returns a stable engine error for invalid options, cancellation, timeout,
    /// output bounds, backpressure closure, or inference failure.
    pub fn generate(
        &self,
        text: &str,
        options: GenerationOptions,
        cancellation: Arc<AtomicBool>,
        sender: tokio::sync::mpsc::Sender<Vec<u8>>,
    ) -> Result<GenerationSummary, EngineError> {
        let chunks = self.prepare_chunks(text)?;
        let started = Instant::now();
        let digest = Arc::new(Mutex::new(Sha256::new()));
        let mut byte_length = 0_usize;
        let mut frame_count = 0_usize;

        for (index, chunk) in chunks.iter().enumerate() {
            let seed_offset = u32::try_from(index).map_err(|_| EngineError::OutputTooLarge)?;
            let summary = self.synthesize_chunk(
                chunk,
                options.seed.wrapping_add(seed_offset),
                options.timeout,
                options.max_audio_bytes.saturating_sub(byte_length),
                started,
                Arc::clone(&cancellation),
                sender.clone(),
                Arc::clone(&digest),
            )?;
            byte_length = byte_length
                .checked_add(summary.byte_length)
                .ok_or(EngineError::OutputTooLarge)?;
            frame_count = frame_count
                .checked_add(summary.frame_count)
                .ok_or(EngineError::OutputTooLarge)?;
        }
        drop(sender);

        if byte_length == 0 || frame_count == 0 {
            return Err(EngineError::Failed);
        }
        let pcm_sha256 = digest
            .lock()
            .map_err(|_| EngineError::Failed)?
            .clone()
            .finalize()
            .into();
        Ok(GenerationSummary {
            byte_length,
            frame_count,
            pcm_sha256,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn synthesize_chunk(
        &self,
        prepared: &PreparedChunk,
        seed: u32,
        timeout: Duration,
        max_audio_bytes: usize,
        started: Instant,
        cancellation: Arc<AtomicBool>,
        sender: tokio::sync::mpsc::Sender<Vec<u8>>,
        digest: Arc<Mutex<Sha256>>,
    ) -> Result<ChunkSummary, EngineError> {
        check_stop(&cancellation, started, timeout)?;
        let mut tts_state = self.base_tts_state.clone();
        self.model
            .prompt_text(&mut tts_state, &prepared.tokens)
            .map_err(|_| EngineError::Failed)?;
        let mut rng = NormalRng::new(self.behavior.temperature, seed)?;
        let mut previous = Tensor::from_vec(vec![f32::NAN; self.ldim], (1, 1, self.ldim), &xn::CPU)
            .and_then(|value| value.to::<<Q80F32 as BackendQ>::T>())
            .map_err(|_| EngineError::Failed)?;
        let mut eos_countdown = None;
        let (latent_sender, latent_receiver) = mpsc::sync_channel(PIPELINE_CAPACITY);
        let model = Arc::clone(&self.model);
        let mut mimi_state = self.base_mimi_state.clone();
        let output_gain = self.behavior.output_gain;
        let decoder_cancel = Arc::clone(&cancellation);
        let decoder = std::thread::spawn(move || -> Result<ChunkSummary, EngineError> {
            let mut byte_length = 0_usize;
            let mut frame_count = 0_usize;
            while let Ok(latent) = latent_receiver.recv() {
                check_stop(&decoder_cancel, started, timeout)?;
                let audio = model
                    .decode_latent(&latent, &mut mimi_state)
                    .map_err(|_| EngineError::Failed)?;
                let mut samples = audio
                    .narrow(0, ..1)
                    .and_then(|value| value.contiguous())
                    .and_then(|value| value.to_vec())
                    .map_err(|_| EngineError::Failed)?;
                samples.iter_mut().for_each(|sample| *sample *= output_gain);
                let pcm = audio::floats_to_pcm16(&samples).map_err(|_| EngineError::Failed)?;
                if pcm.is_empty() || pcm.len() > max_audio_bytes.saturating_sub(byte_length) {
                    return Err(EngineError::OutputTooLarge);
                }
                check_stop(&decoder_cancel, started, timeout)?;
                digest.lock().map_err(|_| EngineError::Failed)?.update(&pcm);
                byte_length += pcm.len();
                frame_count += 1;
                sender
                    .blocking_send(pcm)
                    .map_err(|_| EngineError::Cancelled)?;
            }
            Ok(ChunkSummary {
                byte_length,
                frame_count,
            })
        });

        let mut generation_error = None;
        for _ in 0..prepared.max_frames {
            if let Err(error) = check_stop(&cancellation, started, timeout) {
                generation_error = Some(error);
                break;
            }
            let (next, is_eos) = match self
                .model
                .generate_step(&mut tts_state, &previous, &mut rng)
            {
                Ok(step) => step,
                Err(_) => {
                    generation_error = Some(EngineError::Failed);
                    break;
                }
            };
            if latent_sender.send(next.clone()).is_err() {
                break;
            }
            if is_eos && eos_countdown.is_none() {
                eos_countdown = Some(prepared.frames_after_eos);
            }
            if let Some(countdown) = eos_countdown.as_mut() {
                if *countdown == 0 {
                    break;
                }
                *countdown -= 1;
            }
            previous = next;
        }
        drop(latent_sender);
        let decoded = decoder.join().map_err(|_| EngineError::Failed)?;
        match generation_error {
            Some(error) => Err(error),
            None => decoded,
        }
    }

    fn prepare_chunks(&self, text: &str) -> Result<Vec<PreparedChunk>, EngineError> {
        let fragments = sentence_fragments(text);
        if fragments.is_empty() {
            return Err(EngineError::InvalidOptions);
        }
        let mut chunks = Vec::new();
        let mut current = String::new();
        for fragment in fragments {
            let candidate = if current.is_empty() {
                fragment.clone()
            } else {
                format!("{current} {fragment}")
            };
            if self.prepared_token_count(&candidate)? <= MAX_CHUNK_TOKENS {
                current = candidate;
                continue;
            }
            if !current.is_empty() {
                chunks.push(self.prepare_chunk(&current)?);
                current.clear();
            }
            for word in fragment.split_whitespace() {
                let candidate = if current.is_empty() {
                    word.to_owned()
                } else {
                    format!("{current} {word}")
                };
                if self.prepared_token_count(&candidate)? > MAX_CHUNK_TOKENS {
                    if current.is_empty() {
                        return Err(EngineError::InvalidOptions);
                    }
                    chunks.push(self.prepare_chunk(&current)?);
                    current = word.to_owned();
                } else {
                    current = candidate;
                }
            }
        }
        if !current.is_empty() {
            chunks.push(self.prepare_chunk(&current)?);
        }
        Ok(chunks)
    }

    fn prepared_token_count(&self, text: &str) -> Result<usize, EngineError> {
        let (text, _) = prepare_text_prompt(text, self.behavior)?;
        self.model
            .flow_lm
            .conditioner
            .tokenize(&text)
            .map(|tokens| tokens.len())
            .map_err(|_| EngineError::InvalidOptions)
    }

    fn prepare_chunk(&self, text: &str) -> Result<PreparedChunk, EngineError> {
        let (text, frames_after_eos) = prepare_text_prompt(text, self.behavior)?;
        let tokens = self
            .model
            .flow_lm
            .conditioner
            .tokenize(&text)
            .map_err(|_| EngineError::InvalidOptions)?;
        if tokens.is_empty() || tokens.len() > MAX_CHUNK_TOKENS {
            return Err(EngineError::InvalidOptions);
        }
        let max_frames = ((tokens.len() as f64 / 3.0 + 2.0) * 12.5).ceil() as usize;
        Ok(PreparedChunk {
            tokens,
            max_frames,
            frames_after_eos,
        })
    }
}

fn prepare_text_prompt(
    text: &str,
    behavior: XnModelBehavior,
) -> Result<(String, usize), EngineError> {
    let mut text = text.trim().replace(['\n', '\r'], " ").replace("  ", " ");
    if text.is_empty() {
        return Err(EngineError::InvalidOptions);
    }
    if behavior.remove_semicolons {
        text = text.replace(';', ",");
    }
    let frames = if text.split_whitespace().count() <= 4 {
        3
    } else {
        1
    } + behavior.frames_after_eos_offset;
    let mut chars = text.chars();
    if let Some(first) = chars.next()
        && !first.is_uppercase()
    {
        text = first.to_uppercase().to_string() + chars.as_str();
    }
    if text.chars().last().is_some_and(char::is_alphanumeric) {
        text.push('.');
    }
    if behavior.pad_with_spaces_for_short_inputs && text.split_whitespace().count() < 5 {
        text = format!("        {text}");
    }
    Ok((text, frames))
}

fn sentence_fragments(text: &str) -> Vec<String> {
    let mut fragments = Vec::new();
    let mut current = String::new();
    for character in text.trim().chars() {
        current.push(character);
        if matches!(character, '.' | '!' | '?') {
            let fragment = current.trim();
            if !fragment.is_empty() {
                fragments.push(fragment.to_owned());
            }
            current.clear();
        }
    }
    let fragment = current.trim();
    if !fragment.is_empty() {
        fragments.push(fragment.to_owned());
    }
    fragments
}

fn check_stop(
    cancellation: &AtomicBool,
    started: Instant,
    timeout: Duration,
) -> Result<(), EngineError> {
    if cancellation.load(Ordering::Acquire) {
        Err(EngineError::Cancelled)
    } else if started.elapsed() >= timeout {
        Err(EngineError::Timeout)
    } else {
        Ok(())
    }
}

fn check_cancelled(cancelled: &impl Fn() -> bool) -> Result<(), EngineError> {
    if cancelled() {
        Err(EngineError::Cancelled)
    } else {
        Ok(())
    }
}

fn resample<F>(
    input: &[f32],
    input_rate: u32,
    output_rate: u32,
    cancelled: &F,
) -> Result<Vec<f32>, EngineError>
where
    F: Fn() -> bool,
{
    let input_rate = usize::try_from(input_rate).map_err(|_| EngineError::Failed)?;
    let output_rate = usize::try_from(output_rate).map_err(|_| EngineError::Failed)?;
    let expected = input
        .len()
        .checked_mul(output_rate)
        .and_then(|value| value.checked_div(input_rate))
        .and_then(|value| value.checked_add(RESAMPLE_CHUNK_FRAMES))
        .ok_or(EngineError::OutputTooLarge)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(expected)
        .map_err(|_| EngineError::OutputTooLarge)?;
    let mut resampler =
        rubato::FftFixedInOut::<f32>::new(input_rate, output_rate, RESAMPLE_CHUNK_FRAMES, 1)
            .map_err(|_| EngineError::Failed)?;
    let mut buffer = resampler.output_buffer_allocate(true);
    let mut position = 0;
    while position + resampler.input_frames_next() < input.len() {
        check_cancelled(cancelled)?;
        let (consumed, produced) = resampler
            .process_into_buffer(&[&input[position..]], &mut buffer, None)
            .map_err(|_| EngineError::Failed)?;
        position += consumed;
        output.extend_from_slice(&buffer[0][..produced]);
    }
    if position < input.len() {
        check_cancelled(cancelled)?;
        let (_, produced) = resampler
            .process_partial_into_buffer(Some(&[&input[position..]]), &mut buffer, None)
            .map_err(|_| EngineError::Failed)?;
        output.extend_from_slice(&buffer[0][..produced]);
    }
    check_cancelled(cancelled)?;
    Ok(output)
}

fn load_voice(path: &Path) -> xn::Result<Tensor<<Q80F32 as BackendQ>::T, xn::CpuDevice>> {
    let weights = VB::load(&[path], xn::CPU)?;
    let names = weights.tensor_names();
    if names.len() != 1 {
        xn::bail!("voice state must contain one tensor")
    }
    let name = &names[0];
    let Some(shape) = weights.shape(name) else {
        xn::bail!("voice tensor has no shape")
    };
    let voice: Tensor<f32, _> = weights.tensor(name, shape.clone())?;
    let voice = match shape.dims() {
        [frames, channels] => voice.reshape((1, *frames, *channels))?,
        [1, _, _] => voice,
        _ => xn::bail!("unsupported voice state shape"),
    };
    voice.to::<<Q80F32 as BackendQ>::T>()
}

fn remap_key(name: &str) -> Option<String> {
    if name.contains("flow.w_s_t")
        || name.contains("quantizer.vq")
        || name.contains("quantizer.logvar_proj")
    {
        return None;
    }
    Some(
        name.replace(
            "flow_lm.condition_provider.conditioners.speaker_wavs.output_proj.weight",
            "flow_lm.speaker_proj_weight",
        )
        .replace(
            "flow_lm.condition_provider.conditioners.transcript_in_segment.",
            "flow_lm.conditioner.",
        )
        .replace("flow_lm.backbone.", "flow_lm.transformer.")
        .replace("flow_lm.flow.", "flow_lm.flow_net.")
        .replace("mimi.model.", "mimi."),
    )
}

fn ignored_weight(name: &str) -> bool {
    name == "flow_lm.condition_provider.conditioners.speaker_wavs.learnt_padding"
        || name.starts_with("mimi.quantizer")
        || name.starts_with("mimi.encoder")
        || name == "flow_lm.speaker_proj_weight"
        || name == "mimi.downsample.conv.conv.weight"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn april_text_policy_matches_the_official_defaults() {
        let behavior = XnModelBehavior::april_2026_english();
        assert_eq!(
            prepare_text_prompt("  hello world  ", behavior).unwrap(),
            ("Hello world.".to_owned(), 5)
        );
        assert_eq!(behavior.output_gain, 0.65);
    }

    #[test]
    fn sentence_splitting_preserves_terminal_punctuation() {
        assert_eq!(
            sentence_fragments("One. Two? Three! Four"),
            ["One.", "Two?", "Three!", "Four"]
        );
    }

    #[test]
    fn behavior_validation_rejects_unbounded_values() {
        let mut behavior = XnModelBehavior::april_2026_english();
        behavior.frames_after_eos_offset = 65;
        assert_eq!(behavior.validate(), Err(EngineError::InvalidOptions));
        behavior.frames_after_eos_offset = 2;
        behavior.output_gain = 0.0;
        assert_eq!(behavior.validate(), Err(EngineError::InvalidOptions));
        behavior.output_gain = 1.01;
        assert_eq!(behavior.validate(), Err(EngineError::InvalidOptions));
    }
}
