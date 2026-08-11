use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use sha2::{Digest, Sha256};
use utterpipe_pocket_tts::{
    engine::{EngineError, GenerationOptions},
    xn_engine::{XnModelBehavior, XnPocketEngine},
};

fn fixture(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {name} to an absolute fixture path"))
}

fn engine() -> Arc<XnPocketEngine> {
    Arc::new(
        XnPocketEngine::create(
            &fixture("UTTERPIPE_POCKET_XN_CONFIG"),
            &fixture("UTTERPIPE_POCKET_XN_MODEL"),
            &fixture("UTTERPIPE_POCKET_XN_TOKENIZER"),
            &fixture("UTTERPIPE_POCKET_XN_VOICE_STATE"),
            XnModelBehavior::april_2026_english(),
            2,
        )
        .unwrap(),
    )
}

fn options(max_audio_bytes: usize) -> GenerationOptions {
    GenerationOptions {
        speed: 1.0,
        seed: 42,
        timeout: Duration::from_secs(30),
        max_audio_bytes,
    }
}

#[tokio::test]
#[ignore = "requires the pinned April XN model, tokenizer, and voice state"]
async fn xn_streams_bounds_and_cancels_with_one_warm_engine() {
    let engine = engine();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let synthesis_engine = Arc::clone(&engine);
    let synthesis = tokio::task::spawn_blocking(move || {
        synthesis_engine.generate(
            "The bounded XN adapter streams consecutive audio and remains responsive.",
            options(32 * 1_024 * 1_024),
            Arc::new(AtomicBool::new(false)),
            sender,
        )
    });
    let mut pcm = Vec::new();
    while let Some(frame) = receiver.recv().await {
        assert!(!frame.is_empty());
        pcm.extend_from_slice(&frame);
    }
    let summary = synthesis.await.unwrap().unwrap();
    assert_eq!(summary.byte_length, pcm.len());
    assert!(summary.frame_count > 1);
    assert_eq!(summary.pcm_sha256, <[u8; 32]>::from(Sha256::digest(&pcm)));

    let (sender, _receiver) = tokio::sync::mpsc::channel(1);
    let bounded_engine = Arc::clone(&engine);
    let bounded = tokio::task::spawn_blocking(move || {
        bounded_engine.generate(
            "This output cannot fit in one byte.",
            options(1),
            Arc::new(AtomicBool::new(false)),
            sender,
        )
    })
    .await
    .unwrap();
    assert!(matches!(bounded, Err(EngineError::OutputTooLarge)));

    let cancellation = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancellation);
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let cancellation_engine = Arc::clone(&engine);
    let cancelled = tokio::task::spawn_blocking(move || {
        cancellation_engine.generate(
            "Cancel this deliberately long XN synthesis after its first delivered frame so the same warm process can continue serving future work.",
            options(32 * 1_024 * 1_024),
            worker_cancel,
            sender,
        )
    });
    assert!(
        tokio::time::timeout(Duration::from_secs(10), receiver.recv())
            .await
            .unwrap()
            .is_some()
    );
    cancellation.store(true, Ordering::Release);
    while receiver.recv().await.is_some() {}
    assert!(matches!(
        cancelled.await.unwrap(),
        Err(EngineError::Cancelled)
    ));
}
