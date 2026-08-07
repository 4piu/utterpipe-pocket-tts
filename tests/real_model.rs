use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use utterpipe_pocket_tts::{
    engine::{EngineError, EngineOptions, GenerationOptions, PocketEngine},
    model::{LICENSE_IDS, MODEL_ID},
    store::{Store, StoreError},
};

fn fixture(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {name} to an absolute fixture path"))
}

fn reference_fixture(directory: &Path) -> PathBuf {
    if let Some(path) = std::env::var_os("UTTERPIPE_POCKET_REFERENCE_WAV") {
        return PathBuf::from(path);
    }
    let path = directory.join("synthetic-reference.wav");
    let sample_rate = 24_000_u32;
    let sample_count = sample_rate * 4;
    let data_bytes = sample_count * 2;
    let mut wav = Vec::with_capacity(44 + usize::try_from(data_bytes).unwrap());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    for index in 0..sample_count {
        let phase = i32::try_from(index % 160).unwrap();
        let triangle = if phase < 80 {
            -12_000 + phase * 300
        } else {
            12_000 - (phase - 80) * 300
        };
        let envelope = i32::try_from((index / 2_400) % 4 + 1).unwrap();
        let sample = i16::try_from(triangle / envelope).unwrap();
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    fs::write(&path, wav).unwrap();
    path
}

#[tokio::test]
#[ignore = "requires the pinned 94 MiB model archive"]
async fn real_inference_streams_cancels_and_honors_shared_leases() {
    let archive = fixture("UTTERPIPE_POCKET_MODEL_ARCHIVE");
    let temp = TempDir::new().unwrap();
    let reference = reference_fixture(temp.path());
    assert!(archive.is_absolute() && archive.is_file());
    assert!(reference.is_absolute() && reference.is_file());

    let store = Store::new(temp.path().join("data"), temp.path().join("cache")).unwrap();
    let accepted = LICENSE_IDS
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    store
        .install_model_from_archive(&archive, &accepted)
        .unwrap();
    store
        .import_voice(&reference, "bria-local", true, 30.0)
        .unwrap();

    let first_assets = store.acquire_runtime(MODEL_ID, "bria-local").unwrap();
    let second_assets = store.acquire_runtime(MODEL_ID, "bria-local").unwrap();
    assert!(matches!(
        store.remove_artifacts(&[format!("model:{MODEL_ID}"), "voice:bria-local".to_owned()]),
        Err(StoreError::ResourceBusy)
    ));
    drop(second_assets);

    let engine =
        Arc::new(PocketEngine::create(&first_assets.model_dir, EngineOptions::default()).unwrap());
    let prompt = first_assets.reference.clone();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let generation_engine = Arc::clone(&engine);
    let generation = tokio::task::spawn_blocking(move || {
        generation_engine.generate(
            "This genuine Pocket synthesis is deliberately long enough to exercise several consecutive callback frames before it reaches the terminal result.",
            &prompt,
            GenerationOptions { speed:1.0, seed:42, timeout:Duration::from_secs(90), max_audio_bytes:32 * 1_024 * 1_024 },
            Arc::new(AtomicBool::new(false)),
            sender,
        )
    });
    let first = tokio::time::timeout(Duration::from_secs(60), receiver.recv())
        .await
        .unwrap()
        .expect("first incremental callback");
    assert!(!first.is_empty());
    assert!(
        !generation.is_finished(),
        "generation completed before the bounded callback stream was consumed"
    );
    let mut pcm = first;
    while let Some(chunk) = receiver.recv().await {
        pcm.extend_from_slice(&chunk);
    }
    let summary = generation.await.unwrap().unwrap();
    assert_eq!(summary.byte_length, pcm.len());
    assert!(summary.frame_count > 1);
    let callback_digest: [u8; 32] = Sha256::digest(&pcm).into();
    assert_eq!(summary.pcm_sha256, callback_digest);
    assert!(pcm.chunks_exact(2).any(|sample| sample != [0, 0]));

    let prompt = first_assets.reference.clone();
    let bounded_engine = Arc::clone(&engine);
    let (sender, _receiver) = tokio::sync::mpsc::channel(1);
    let bounded = tokio::task::spawn_blocking(move || {
        bounded_engine.generate(
            "This request must stop at the first native callback because one byte cannot hold even one PCM16 sample.",
            &prompt,
            GenerationOptions { speed:1.0, seed:42, timeout:Duration::from_secs(90), max_audio_bytes:1 },
            Arc::new(AtomicBool::new(false)),
            sender,
        )
    })
    .await
    .unwrap();
    assert!(matches!(bounded, Err(EngineError::OutputTooLarge)));

    let cancellation = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancellation);
    let prompt = first_assets.reference.clone();
    let cancellation_engine = Arc::clone(&engine);
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let cancelled_generation = tokio::task::spawn_blocking(move || {
        cancellation_engine.generate(
            "Cancel this long Pocket synthesis after its first genuine callback frame, while the same warm engine instance remains loaded.",
            &prompt,
            GenerationOptions { speed:1.0, seed:42, timeout:Duration::from_secs(90), max_audio_bytes:32 * 1_024 * 1_024 },
            worker_cancel,
            sender,
        )
    });
    assert!(
        tokio::time::timeout(Duration::from_secs(60), receiver.recv())
            .await
            .unwrap()
            .is_some()
    );
    cancellation.store(true, Ordering::Release);
    while receiver.recv().await.is_some() {}
    assert!(matches!(
        cancelled_generation.await.unwrap(),
        Err(EngineError::Cancelled)
    ));
}
