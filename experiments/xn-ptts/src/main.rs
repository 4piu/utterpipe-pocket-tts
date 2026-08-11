use std::{
    fs::File,
    io::{BufReader, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use ptts::{
    Tokenizer,
    mimi::MimiDecoderState,
    tts_model::{TTSConfig, TTSModel, TTSState, prepare_text_prompt},
};
use rand::SeedableRng;
use serde::Serialize;
use sha2::{Digest, Sha256};
use xn::{
    BackendQ, Tensor, Unquantized,
    nn::VB,
    quantized::{Q4kF32, Q6kF32, Q80F32},
};

const SAMPLE_RATE: usize = 24_000;

#[derive(Clone, Copy, Debug, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum Precision {
    Fp32,
    Q8,
    Q6k,
    Q4k,
}

#[derive(Debug, Parser)]
#[command(about = "Benchmark the native XN Pocket TTS core with prepared local assets")]
struct Args {
    /// XN Pocket TTS JSON configuration.
    #[arg(long)]
    config: PathBuf,
    /// Official safetensors weights for FP32, or an XN GGUF for Q8.
    #[arg(long)]
    weights: PathBuf,
    /// SentencePiece tokenizer matching the model.
    #[arg(long)]
    tokenizer: PathBuf,
    /// Prepared XN voice-state safetensors.
    #[arg(long)]
    voice_state: PathBuf,
    /// Runtime precision represented by --weights.
    #[arg(long, value_enum)]
    precision: Precision,
    /// Text synthesized by every run.
    #[arg(
        long,
        default_value = "The local speech engine is ready for a repeatable benchmark."
    )]
    text: String,
    /// XN CPU worker threads.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u32).range(1..=64))]
    threads: u32,
    /// Unreported warm synthesis runs.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u32).range(0..=100))]
    warmups: u32,
    /// Reported synthesis runs.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..=100))]
    iterations: u32,
    /// Separate attempts cancelled after the first decoded audio frame.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u32).range(0..=100))]
    cancellation_iterations: u32,
    /// Sampling temperature.
    #[arg(long, default_value_t = 0.7)]
    temperature: f32,
    /// Sampling seed reset before every synthesis.
    #[arg(long, default_value_t = 42)]
    seed: u64,
    /// Optionally save the final measured synthesis as PCM16 WAV.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Serialize)]
struct RunMeasurement {
    iteration: u32,
    first_audio_ms: f64,
    completion_ms: f64,
    audio_seconds: f64,
    real_time_factor: f64,
    pcm_bytes: usize,
    decoded_frames: usize,
    pcm_sha256: String,
}

#[derive(Serialize)]
struct CancellationMeasurement {
    iteration: u32,
    first_audio_ms: f64,
    completion_ms: f64,
    decoded_frames: usize,
    pcm_bytes_before_stop: usize,
}

struct SynthesisResult {
    first_audio: Duration,
    completion: Duration,
    chunks: Vec<Vec<f32>>,
}

struct Prepared<Q: BackendQ> {
    model: Arc<TTSModel<Q>>,
    base_tts_state: TTSState<Q>,
    base_mimi_state: MimiDecoderState<f32, Q::B>,
    tokens: Vec<u32>,
    max_frames: usize,
    frames_after_eos: usize,
    ldim: usize,
    device: Q::B,
}

struct NormalRng {
    inner: rand::rngs::StdRng,
    distribution: rand_distr::Normal<f32>,
}

impl NormalRng {
    fn new(temperature: f32, seed: u64) -> Result<Self> {
        if !temperature.is_finite() || temperature <= 0.0 {
            bail!("temperature must be finite and greater than zero");
        }
        Ok(Self {
            inner: rand::rngs::StdRng::seed_from_u64(seed),
            distribution: rand_distr::Normal::new(0.0, temperature.sqrt())?,
        })
    }
}

impl ptts::flow_lm::Rng for NormalRng {
    fn sample(&mut self) -> f32 {
        use rand::Rng as _;
        self.inner.sample(self.distribution)
    }
}

struct SpTokenizer(sentencepiece::SentencePieceProcessor);

impl ptts::Tokenizer for SpTokenizer {
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

fn main() -> Result<()> {
    let args = Args::parse();
    validate_paths(&args)?;
    xn::set_num_threads(args.threads as usize);
    match args.precision {
        Precision::Fp32 => run::<Unquantized<f32, xn::CpuDevice>>(&args, xn::CPU),
        Precision::Q8 => run::<Q80F32>(&args, xn::CPU),
        Precision::Q6k => run::<Q6kF32>(&args, xn::CPU),
        Precision::Q4k => run::<Q4kF32>(&args, xn::CPU),
    }
}

fn run<Q>(args: &Args, device: Q::B) -> Result<()>
where
    Q: BackendQ + 'static,
{
    let config: TTSConfig = serde_json::from_reader(BufReader::new(File::open(&args.config)?))?;
    if config.mimi.sample_rate != SAMPLE_RATE {
        bail!("evaluation currently requires a 24 kHz model");
    }
    let tokenizer_path = args
        .tokenizer
        .to_str()
        .context("tokenizer path is not valid UTF-8")?;
    let tokenizer = SpTokenizer(sentencepiece::SentencePieceProcessor::open(tokenizer_path)?);

    let prepared_text = prepare_text_prompt(&args.text);
    let tokens = tokenizer.encode(&prepared_text.0)?;
    let max_frames = ((tokens.len() as f64 / 3.0 + 2.0) * 12.5).ceil() as usize;
    let sequence_budget = tokens.len() + 512 + max_frames;

    let load_started = Instant::now();
    let vb = load_weights(&args.weights, device.clone())?;
    let root = vb.root();
    let model = TTSModel::<Q>::load(&root, Box::new(tokenizer), &config)?;
    root.check_all_used_with_ignore(ignored_weight)?;
    let engine_load_ms = milliseconds(load_started.elapsed());

    let voice_started = Instant::now();
    let mut base_tts_state = model.init_flow_lm_state(1, sequence_budget)?;
    let voice = load_voice::<Q>(&args.voice_state, device.clone())?;
    model.prompt_audio(&mut base_tts_state, &voice)?;
    let base_mimi_state = model.init_mimi_state(1, 250)?;
    let voice_state_ms = milliseconds(voice_started.elapsed());

    let prepared = Prepared {
        model: Arc::new(model),
        base_tts_state,
        base_mimi_state,
        tokens,
        max_frames,
        frames_after_eos: prepared_text.1,
        ldim: config.flow_lm.ldim,
        device,
    };

    for _ in 0..args.warmups {
        synthesize(&prepared, args.temperature, args.seed, false)?;
    }

    let mut runs = Vec::with_capacity(args.iterations as usize);
    let mut saved_pcm = None;
    for iteration in 1..=args.iterations {
        let result = synthesize(&prepared, args.temperature, args.seed, false)?;
        let pcm = chunks_to_pcm16(&result.chunks)?;
        let audio_seconds = pcm.len() as f64 / 2.0 / SAMPLE_RATE as f64;
        let completion_ms = milliseconds(result.completion);
        runs.push(RunMeasurement {
            iteration,
            first_audio_ms: milliseconds(result.first_audio),
            completion_ms,
            audio_seconds,
            real_time_factor: completion_ms / 1_000.0 / audio_seconds,
            pcm_bytes: pcm.len(),
            decoded_frames: result.chunks.len(),
            pcm_sha256: format!("{:x}", Sha256::digest(&pcm)),
        });
        saved_pcm = Some(pcm);
    }

    let mut cancellations = Vec::with_capacity(args.cancellation_iterations as usize);
    for iteration in 1..=args.cancellation_iterations {
        let result = synthesize(&prepared, args.temperature, args.seed, true)?;
        let pcm = chunks_to_pcm16(&result.chunks)?;
        cancellations.push(CancellationMeasurement {
            iteration,
            first_audio_ms: milliseconds(result.first_audio),
            completion_ms: milliseconds(result.completion),
            decoded_frames: result.chunks.len(),
            pcm_bytes_before_stop: pcm.len(),
        });
    }

    if let (Some(path), Some(pcm)) = (&args.output, saved_pcm.as_deref()) {
        write_wav(path, pcm)?;
    }

    let first_audio = runs
        .iter()
        .map(|run| run.first_audio_ms)
        .collect::<Vec<_>>();
    let completion = runs.iter().map(|run| run.completion_ms).collect::<Vec<_>>();
    let real_time_factor = runs
        .iter()
        .map(|run| run.real_time_factor)
        .collect::<Vec<_>>();
    let output = serde_json::json!({
        "schema": "utterpipe_pocket_tts.xn_runtime_benchmark/1",
        "runtime": {
            "name": "xn-ptts",
            "ptts_version": "0.2.2",
            "xn_version": "0.1.21",
            "precision": args.precision,
        },
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "logical_cpus": std::thread::available_parallelism().map_or(1, usize::from),
        },
        "configuration": {
            "threads": args.threads,
            "warmup_runs": args.warmups,
            "measured_runs": args.iterations,
            "cancellation_runs": args.cancellation_iterations,
            "text_unicode_scalars": args.text.chars().count(),
            "text_tokens": prepared.tokens.len(),
            "temperature": args.temperature,
            "seed": args.seed,
            "sample_rate_hz": SAMPLE_RATE,
            "pipeline_capacity_frames": 2,
        },
        "engine_load_ms": engine_load_ms,
        "voice_state_prompt_ms": voice_state_ms,
        "peak_rss_bytes": peak_rss_bytes(),
        "summary": {
            "first_audio_ms_p50": percentile(&first_audio, 0.50),
            "first_audio_ms_p95": percentile(&first_audio, 0.95),
            "completion_ms_p50": percentile(&completion, 0.50),
            "completion_ms_p95": percentile(&completion, 0.95),
            "real_time_factor_p50": percentile(&real_time_factor, 0.50),
            "real_time_factor_p95": percentile(&real_time_factor, 0.95),
        },
        "runs": runs,
        "cancellations": cancellations,
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn synthesize<Q>(
    prepared: &Prepared<Q>,
    temperature: f32,
    seed: u64,
    cancel_after_first_audio: bool,
) -> Result<SynthesisResult>
where
    Q: BackendQ + 'static,
{
    let started = Instant::now();
    let mut tts_state = prepared.base_tts_state.clone();
    prepared
        .model
        .prompt_text(&mut tts_state, &prepared.tokens)?;
    let mut rng = NormalRng::new(temperature, seed)?;
    let mut previous = Tensor::from_vec(
        vec![f32::NAN; prepared.ldim],
        (1, 1, prepared.ldim),
        &prepared.device,
    )?
    .to::<Q::T>()?;
    let mut eos_countdown = None;

    let (latent_sender, latent_receiver) = mpsc::sync_channel(2);
    let first_audio_ready = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));
    let decoder = {
        let model = Arc::clone(&prepared.model);
        let mut mimi_state = prepared.base_mimi_state.clone();
        let first_audio_ready = Arc::clone(&first_audio_ready);
        let cancelled = Arc::clone(&cancelled);
        std::thread::spawn(move || -> Result<(Duration, Vec<Vec<f32>>)> {
            let mut first_audio = None;
            let mut chunks = Vec::new();
            while let Ok(latent) = latent_receiver.recv() {
                if cancelled.load(Ordering::Acquire) {
                    break;
                }
                let audio = model.decode_latent(&latent, &mut mimi_state)?;
                if cancelled.load(Ordering::Acquire) {
                    break;
                }
                let chunk = audio.narrow(0, ..1)?.contiguous()?.to_vec()?;
                if chunk.is_empty() {
                    bail!("XN decoded an empty audio frame");
                }
                if first_audio.is_none() {
                    first_audio = Some(started.elapsed());
                    first_audio_ready.store(true, Ordering::Release);
                }
                chunks.push(chunk);
            }
            Ok((first_audio.context("XN returned no decoded audio")?, chunks))
        })
    };

    for _ in 0..prepared.max_frames {
        if cancel_after_first_audio && first_audio_ready.load(Ordering::Acquire) {
            cancelled.store(true, Ordering::Release);
            break;
        }
        let (next, is_eos) = prepared
            .model
            .generate_step(&mut tts_state, &previous, &mut rng)?;
        latent_sender
            .send(next.clone())
            .map_err(|_| anyhow::anyhow!("XN decoder stopped before generation"))?;
        if cancel_after_first_audio && first_audio_ready.load(Ordering::Acquire) {
            cancelled.store(true, Ordering::Release);
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
    let (first_audio, chunks) = decoder
        .join()
        .map_err(|_| anyhow::anyhow!("XN decoder thread panicked"))??;
    if cancel_after_first_audio && !cancelled.load(Ordering::Acquire) {
        bail!("synthesis completed before after-first-audio cancellation was observed");
    }
    Ok(SynthesisResult {
        first_audio,
        completion: started.elapsed(),
        chunks,
    })
}

fn load_weights<B: xn::Backend>(path: &Path, device: B) -> Result<VB<B>> {
    if path.extension().and_then(|extension| extension.to_str()) == Some("gguf") {
        VB::load_gguf_with_key_map(BufReader::new(File::open(path)?), device, remap_key)
            .map_err(Into::into)
    } else {
        VB::load_with_key_map(&[path], device, remap_key).map_err(Into::into)
    }
}

fn load_voice<Q: BackendQ>(path: &Path, device: Q::B) -> Result<Tensor<Q::T, Q::B>> {
    let vb = VB::load(&[path], device)?;
    let names = vb.tensor_names();
    if names.len() != 1 {
        bail!("voice state must contain exactly one tensor");
    }
    let name = &names[0];
    let shape = vb.shape(name).context("voice tensor has no shape")?;
    let dimensions = shape.dims();
    let voice: Tensor<f32, Q::B> = vb.tensor(name, shape.clone())?;
    let voice = match dimensions {
        [frames, channels] => voice.reshape((1, *frames, *channels))?,
        [1, _, _] => voice,
        _ => bail!("voice state tensor has an unsupported shape"),
    };
    Ok(voice.to::<Q::T>()?)
}

fn validate_paths(args: &Args) -> Result<()> {
    for (label, path) in [
        ("config", &args.config),
        ("weights", &args.weights),
        ("tokenizer", &args.tokenizer),
        ("voice state", &args.voice_state),
    ] {
        if !path.is_file() {
            bail!("{label} is not a regular file: {}", path.display());
        }
    }
    match (
        args.precision,
        args.weights.extension().and_then(|value| value.to_str()),
    ) {
        (Precision::Q8 | Precision::Q6k | Precision::Q4k, Some("gguf"))
        | (Precision::Fp32, Some("safetensors")) => Ok(()),
        (Precision::Q8 | Precision::Q6k | Precision::Q4k, _) => {
            bail!("quantized evaluation requires XN GGUF weights")
        }
        (Precision::Fp32, _) => bail!("FP32 evaluation requires safetensors weights"),
    }
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

fn chunks_to_pcm16(chunks: &[Vec<f32>]) -> Result<Vec<u8>> {
    let sample_count = chunks.iter().map(Vec::len).sum::<usize>();
    let mut pcm = Vec::with_capacity(sample_count * 2);
    for &sample in chunks.iter().flatten() {
        if !sample.is_finite() {
            bail!("XN produced a non-finite sample");
        }
        let sample = sample.clamp(-1.0, 1.0);
        let value = if sample <= -1.0 {
            i16::MIN
        } else if sample >= 1.0 {
            i16::MAX
        } else {
            (sample * 32_767.0).round() as i16
        };
        pcm.extend_from_slice(&value.to_le_bytes());
    }
    Ok(pcm)
}

fn write_wav(path: &Path, pcm: &[u8]) -> Result<()> {
    let payload_size = u32::try_from(pcm.len()).context("WAV payload is too large")?;
    let riff_size = 36_u32
        .checked_add(payload_size)
        .context("WAV is too large")?;
    let mut output = File::create(path)?;
    output.write_all(b"RIFF")?;
    output.write_all(&riff_size.to_le_bytes())?;
    output.write_all(b"WAVEfmt ")?;
    output.write_all(&16_u32.to_le_bytes())?;
    output.write_all(&1_u16.to_le_bytes())?;
    output.write_all(&1_u16.to_le_bytes())?;
    output.write_all(&(SAMPLE_RATE as u32).to_le_bytes())?;
    output.write_all(&((SAMPLE_RATE as u32) * 2).to_le_bytes())?;
    output.write_all(&2_u16.to_le_bytes())?;
    output.write_all(&16_u16.to_le_bytes())?;
    output.write_all(b"data")?;
    output.write_all(&payload_size.to_le_bytes())?;
    output.write_all(pcm)?;
    Ok(())
}

fn percentile(values: &[f64], percentile: f64) -> f64 {
    let mut ordered = values.to_vec();
    ordered.sort_by(f64::total_cmp);
    let index = ((ordered.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(ordered.len() - 1);
    ordered[index]
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[cfg(unix)]
fn peak_rss_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::uninit();
    // SAFETY: getrusage initializes the complete rusage struct on success.
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    // SAFETY: the successful call above initialized usage.
    let maximum = unsafe { usage.assume_init() }.ru_maxrss;
    u64::try_from(maximum).ok().map(|value| {
        if cfg!(target_os = "macos") {
            value
        } else {
            value.saturating_mul(1_024)
        }
    })
}

#[cfg(windows)]
fn peak_rss_bytes() -> Option<u64> {
    use windows_sys::Win32::System::{
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::GetCurrentProcess,
    };

    let size = u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>()).ok()?;
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: size,
        ..PROCESS_MEMORY_COUNTERS::default()
    };
    // SAFETY: GetCurrentProcess returns a valid pseudo-handle for this process,
    // and counters points to a complete writable structure of the supplied size.
    let success = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
    (success != 0).then(|| counters.PeakWorkingSetSize as u64)
}

#[cfg(not(any(unix, windows)))]
fn peak_rss_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_nearest_rank() {
        assert_eq!(percentile(&[4.0, 1.0, 3.0, 2.0], 0.50), 2.0);
        assert_eq!(percentile(&[4.0, 1.0, 3.0, 2.0], 0.95), 4.0);
    }

    #[test]
    fn pcm_conversion_matches_provider_edges() {
        assert_eq!(
            chunks_to_pcm16(&[vec![-1.0, 0.0, 1.0]]).unwrap(),
            [
                i16::MIN.to_le_bytes(),
                0_i16.to_le_bytes(),
                i16::MAX.to_le_bytes()
            ]
            .concat()
        );
    }
}
