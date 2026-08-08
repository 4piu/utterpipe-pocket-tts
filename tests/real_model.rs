use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use utterpipe_pocket_tts::{
    engine::{EngineError, EngineOptions, GenerationOptions, PocketEngine},
    model::{LICENSE_IDS, MODEL_ID},
    store::{Store, StoreError},
};

const CONTROL: u8 = 0x01;
const AUDIO: u8 = 0x02;

struct ProviderProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl ProviderProcess {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_utterpipe-pocket-tts"))
            .args(["protocol", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn Pocket provider");
        Self {
            stdin: child.stdin.take().expect("provider stdin"),
            stdout: child.stdout.take().expect("provider stdout"),
            child,
        }
    }

    fn request(&mut self, id: &str, method: &str, params: Value) {
        let payload = serde_json::to_vec(&json!({
            "kind":"request", "id":id, "method":method, "params":params
        }))
        .unwrap();
        write_frame(&mut self.stdin, CONTROL, &payload);
    }

    fn control(&mut self) -> Value {
        let (kind, payload) = read_frame(&mut self.stdout);
        assert_eq!(kind, CONTROL);
        serde_json::from_slice(&payload).unwrap()
    }

    fn shutdown(mut self) {
        self.request("shutdown", "session.shutdown", json!({}));
        assert_eq!(self.control()["result"]["accepted"], true);
        drop(self.stdin);
        assert!(self.child.wait().unwrap().success());
    }
}

fn write_frame(writer: &mut impl Write, kind: u8, payload: &[u8]) {
    let mut header = [0_u8; 12];
    header[..4].copy_from_slice(b"UTP1");
    header[4] = kind;
    header[8..].copy_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
    writer.write_all(&header).unwrap();
    writer.write_all(payload).unwrap();
    writer.flush().unwrap();
}

fn read_frame(reader: &mut impl Read) -> (u8, Vec<u8>) {
    let mut header = [0_u8; 12];
    reader.read_exact(&mut header).unwrap();
    assert_eq!(&header[..4], b"UTP1");
    assert_eq!(&header[5..8], &[0, 0, 0]);
    let length = u32::from_be_bytes(header[8..12].try_into().unwrap()) as usize;
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).unwrap();
    (header[4], payload)
}

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
    drop(engine);
    drop(first_assets);

    protocol_incremental_smoke(&temp);
}

fn protocol_incremental_smoke(temp: &TempDir) {
    let mut provider = ProviderProcess::spawn();
    provider.request(
        "hello",
        "protocol.hello",
        json!({
            "protocol":"utterpipe.tts",
            "versions":[1],
            "expected_provider":"pocket-tts",
            "session":"runtime",
            "utterance_schema_profiles":["utterpipe.utterance-options/1"],
            "host":{"name":"real-model-test", "version":"0.1.0"}
        }),
    );
    let hello = provider.control();
    assert_eq!(hello["result"]["version"], 1);
    assert_eq!(hello["result"]["provider"]["slug"], "pocket-tts");

    provider.request(
        "init",
        "session.initialize",
        json!({
            "data_dir":temp.path().join("data").to_string_lossy(),
            "cache_dir":temp.path().join("cache").to_string_lossy(),
            "provider_options":{
                "model":MODEL_ID,
                "voice":"bria-local",
                "num_threads":2
            },
            "limits":{
                "max_text_code_points":500,
                "max_audio_bytes":16_777_216,
                "synthesis_timeout_ms":90_000
            },
            "accepted_audio_deliveries":[
                {"mode":"incremental", "format":"audio/pcm;codec=pcm_s16le"}
            ]
        }),
    );
    let initialized = provider.control();
    assert_eq!(initialized["result"]["ready"], true);
    assert_eq!(
        initialized["result"]["audio_deliveries"],
        json!([{"mode":"incremental", "format":"audio/pcm;codec=pcm_s16le"}])
    );
    assert!(
        initialized["result"]["utterance_options_schema_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:"))
    );

    provider.request(
        "synth",
        "synthesis.start",
        json!({
            "text":"Pocket TTS protocol version two streams this real synthesis.",
            "audio_delivery":{"mode":"incremental", "format":"audio/pcm;codec=pcm_s16le"},
            "utterance_options":{"speed":1.1, "seed":43}
        }),
    );
    let mut pcm = Vec::new();
    let mut began = false;
    loop {
        let (kind, payload) = read_frame(&mut provider.stdout);
        if kind == AUDIO {
            assert!(began);
            pcm.extend_from_slice(&payload);
            continue;
        }
        assert_eq!(kind, CONTROL);
        let control: Value = serde_json::from_slice(&payload).unwrap();
        if control["kind"] == "event" {
            assert_eq!(control["event"], "synthesis.audio_begin");
            assert_eq!(control["params"]["request_id"], "synth");
            began = true;
            continue;
        }
        assert_eq!(control["id"], "synth");
        assert!(control.get("error").is_none(), "{control}");
        assert_eq!(
            control["result"]["audio"]["byte_length"].as_u64(),
            Some(u64::try_from(pcm.len()).unwrap())
        );
        assert_eq!(control["result"]["audio"]["sample_rate_hz"], 24_000);
        break;
    }
    assert!(began && pcm.len() > 2);
    assert!(pcm.chunks_exact(2).any(|sample| sample != [0, 0]));
    provider.shutdown();
}
