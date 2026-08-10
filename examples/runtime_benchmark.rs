use std::{
    io,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

use clap::Parser;
use serde_json::{Value, json};
use utterpipe_pocket_tts::{
    direct_storage::resolve_direct_storage,
    engine::{EngineOptions, GenerationOptions, PocketEngine, SAMPLE_RATE},
    model::MODEL_ID,
    store::Store,
};

#[derive(Parser)]
#[command(about = "Benchmark an already-prepared local Pocket TTS runtime")]
struct Args {
    /// Installed model ID.
    #[arg(long, default_value = MODEL_ID)]
    model: String,
    /// Installed reference-voice ID.
    #[arg(long)]
    voice: String,
    /// Text synthesized by every measured iteration.
    #[arg(
        long,
        default_value = "The local speech engine is ready for a repeatable benchmark."
    )]
    text: String,
    /// Unmeasured warm synthesis runs.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(0..=100))]
    warmup: u32,
    /// Measured warm synthesis runs.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..=100))]
    iterations: u32,
    /// CPU inference threads.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u32).range(1..=64))]
    threads: u32,
    /// Override the platform-standard provider data directory.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Override the platform-standard provider cache directory.
    #[arg(long)]
    cache_dir: Option<PathBuf>,
}

struct Measurement {
    first_audio_ms: f64,
    total_ms: f64,
    audio_seconds: f64,
    real_time_factor: f64,
    bytes: usize,
    frames: usize,
    pcm_sha256: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let storage = resolve_direct_storage(args.data_dir, args.cache_dir)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let store = Store::new(storage.data_dir, storage.cache_dir)?;
    let assets = store.acquire_runtime(&args.model, &args.voice)?;
    let engine_started = Instant::now();
    let engine = Arc::new(PocketEngine::create(
        &assets.model_dir,
        EngineOptions {
            num_threads: args.threads,
            ..EngineOptions::default()
        },
    )?);
    let engine_load_ms = milliseconds(engine_started.elapsed());

    for _ in 0..args.warmup {
        measure(
            Arc::clone(&engine),
            assets.reference.clone(),
            args.text.clone(),
        )
        .await?;
    }

    let mut measurements = Vec::with_capacity(args.iterations as usize);
    for _ in 0..args.iterations {
        measurements.push(
            measure(
                Arc::clone(&engine),
                assets.reference.clone(),
                args.text.clone(),
            )
            .await?,
        );
    }

    let first_audio: Vec<_> = measurements
        .iter()
        .map(|measurement| measurement.first_audio_ms)
        .collect();
    let total: Vec<_> = measurements
        .iter()
        .map(|measurement| measurement.total_ms)
        .collect();
    let real_time_factors: Vec<_> = measurements
        .iter()
        .map(|measurement| measurement.real_time_factor)
        .collect();
    let runs: Vec<Value> = measurements
        .iter()
        .enumerate()
        .map(|(index, measurement)| {
            json!({
                "iteration": index + 1,
                "first_audio_ms": measurement.first_audio_ms,
                "total_ms": measurement.total_ms,
                "audio_seconds": measurement.audio_seconds,
                "real_time_factor": measurement.real_time_factor,
                "byte_length": measurement.bytes,
                "callback_frames": measurement.frames,
                "pcm_sha256": measurement.pcm_sha256,
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema_version": 1,
            "runtime": "sherpa-onnx-1.13.4",
            "model": args.model,
            "voice": args.voice,
            "platform": {
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "logical_cpus": std::thread::available_parallelism().map_or(1, usize::from),
            },
            "configuration": {
                "threads": args.threads,
                "warmup_runs": args.warmup,
                "measured_runs": args.iterations,
                "text_unicode_scalars": args.text.chars().count(),
                "sample_rate_hz": SAMPLE_RATE,
            },
            "engine_load_ms": engine_load_ms,
            "summary": {
                "first_audio_ms_p50": percentile(&first_audio, 0.50),
                "first_audio_ms_p95": percentile(&first_audio, 0.95),
                "total_ms_p50": percentile(&total, 0.50),
                "total_ms_p95": percentile(&total, 0.95),
                "real_time_factor_p50": percentile(&real_time_factors, 0.50),
                "real_time_factor_p95": percentile(&real_time_factors, 0.95),
            },
            "runs": runs,
        }))?
    );
    Ok(())
}

async fn measure(
    engine: Arc<PocketEngine>,
    reference: utterpipe_pocket_tts::audio::ReferenceAudio,
    text: String,
) -> Result<Measurement, Box<dyn std::error::Error>> {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
    let started = Instant::now();
    let generation = tokio::task::spawn_blocking(move || {
        engine.generate(
            &text,
            &reference,
            GenerationOptions {
                speed: 1.0,
                seed: 42,
                timeout: Duration::from_secs(300),
                max_audio_bytes: 64 * 1_024 * 1_024,
            },
            Arc::new(AtomicBool::new(false)),
            sender,
        )
    });
    let first = receiver
        .recv()
        .await
        .ok_or_else(|| io::Error::other("generation returned no incremental audio"))?;
    let first_audio_ms = milliseconds(started.elapsed());
    let mut received_bytes = first.len();
    while let Some(chunk) = receiver.recv().await {
        received_bytes += chunk.len();
    }
    let summary = generation.await??;
    let total_ms = milliseconds(started.elapsed());
    if received_bytes != summary.byte_length {
        return Err(
            io::Error::other("incremental byte count differs from the generation summary").into(),
        );
    }
    let audio_seconds = summary.byte_length as f64 / (f64::from(SAMPLE_RATE) * 2.0);
    let pcm_sha256 =
        summary
            .pcm_sha256
            .iter()
            .fold(String::with_capacity(64), |mut output, byte| {
                use std::fmt::Write as _;
                let _ = write!(output, "{byte:02x}");
                output
            });
    Ok(Measurement {
        first_audio_ms,
        total_ms,
        audio_seconds,
        real_time_factor: total_ms / 1_000.0 / audio_seconds,
        bytes: summary.byte_length,
        frames: summary.frame_count,
        pcm_sha256,
    })
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn percentile(values: &[f64], percentile: f64) -> f64 {
    let mut ordered = values.to_vec();
    ordered.sort_by(f64::total_cmp);
    let index = ((ordered.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(ordered.len() - 1);
    ordered[index]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_nearest_rank() {
        let values = [4.0, 1.0, 3.0, 2.0];
        assert_eq!(percentile(&values, 0.50), 2.0);
        assert_eq!(percentile(&values, 0.95), 4.0);
    }
}
