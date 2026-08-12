use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use utterpipe_pocket_tts::{
    audio,
    engine::{EngineError, GenerationOptions},
    store::{Store, StoreError},
    xn_bundle::{VerifiedXnBundle, verify_bundle},
    xn_engine::XnPocketEngine,
    xn_prepare::{RUNTIME_MODEL_SHA256, prepare_bundle},
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
            stdin: child.stdin.take().unwrap(),
            stdout: child.stdout.take().unwrap(),
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

fn engine(bundle: &VerifiedXnBundle, voice_state: &std::path::Path) -> Arc<XnPocketEngine> {
    Arc::new(
        XnPocketEngine::create(
            &bundle.config_path(),
            &bundle.model_path(),
            &bundle.tokenizer_path(),
            voice_state,
            bundle.behavior(),
            2,
        )
        .unwrap(),
    )
}

fn options(max_audio_bytes: usize) -> GenerationOptions {
    GenerationOptions {
        seed: 42,
        timeout: Duration::from_secs(30),
        max_audio_bytes,
    }
}

#[tokio::test]
#[ignore = "requires the exact pinned April source model and tokenizer"]
async fn xn_prepare_reproduces_the_catalog_q8_bundle() {
    let source = fixture("UTTERPIPE_POCKET_XN_SOURCE");
    let storage = tempfile::tempdir().unwrap();
    let prepared = prepare_bundle(
        &storage.path().join("cache"),
        Some(&source),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let bundle = verify_bundle(prepared.path(), || false).unwrap();
    assert_eq!(
        bundle.manifest.files["model.gguf"].sha256,
        RUNTIME_MODEL_SHA256
    );
}

#[tokio::test]
#[ignore = "requires the pinned full April XN bundle and reference WAV"]
async fn xn_streams_bounds_and_cancels_with_one_warm_engine() {
    let source = fixture("UTTERPIPE_POCKET_XN_BUNDLE");
    let source_bundle = verify_bundle(&source, || false).unwrap();
    let storage = tempfile::tempdir().unwrap();
    let store = Store::new(storage.path().join("data"), storage.path().join("cache")).unwrap();
    let accepted: Vec<_> = source_bundle
        .manifest
        .licenses
        .iter()
        .map(|license| license.id.clone())
        .collect();
    store
        .install_xn_bundle_from_directory(&source, &accepted)
        .unwrap();
    assert_eq!(store.xn_model_status(), "installed");
    let reference = audio::read_reference(&fixture("UTTERPIPE_POCKET_XN_REFERENCE"), 30.0)
        .expect("reference must satisfy the provider import policy");
    let reference_path = storage.path().join("reference.wav");
    audio::write_pcm16_wav(&reference_path, reference.sample_rate, &reference.samples).unwrap();
    protocol_management_smoke(&storage, &source_bundle.manifest.model_id, &reference_path);
    store
        .import_voice(&reference_path, "caro-davy", true, 30.0)
        .unwrap();
    store
        .prepare_xn_voice(&source_bundle.manifest.model_id, "caro-davy", 2)
        .unwrap();
    let runtime_assets = store
        .acquire_xn_runtime(&source_bundle.manifest.model_id, "caro-davy")
        .unwrap();
    let engine = engine(&runtime_assets.bundle, &runtime_assets.voice_state);
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

    protocol_cancellation_smoke(&storage, &source_bundle.manifest.model_id);

    let artifacts = vec![
        format!("model:{}", source_bundle.manifest.model_id),
        "voice:caro-davy".to_owned(),
        "voice:management-caro".to_owned(),
    ];
    assert!(matches!(
        store.remove_artifacts(&artifacts),
        Err(StoreError::ResourceBusy)
    ));
    drop(engine);
    drop(runtime_assets);
    assert_eq!(store.remove_artifacts(&artifacts).unwrap(), artifacts);
    assert_eq!(store.xn_model_status(), "available");
    assert!(store.voice_catalog().unwrap().is_empty());
    assert!(
        !storage
            .path()
            .join("data/voice-states/pocket-tts-english-2026-04-q8/caro-davy")
            .exists()
    );
}

fn protocol_cancellation_smoke(storage: &tempfile::TempDir, model_id: &str) {
    let mut provider = ProviderProcess::spawn();
    provider.request(
        "hello",
        "protocol.hello",
        json!({
            "protocol":"utterpipe.tts", "versions":[1],
            "expected_provider":"pocket-tts", "session":"runtime",
            "utterance_schema_profiles":["utterpipe.utterance-options/1"],
            "host":{"name":"xn-real-model-test", "version":"0.1.0"}
        }),
    );
    assert_eq!(provider.control()["result"]["version"], 1);
    provider.request(
        "init",
        "session.initialize",
        json!({
            "data_dir":storage.path().join("data").to_string_lossy(),
            "cache_dir":storage.path().join("cache").to_string_lossy(),
            "provider_options":{"model":model_id, "voice":"caro-davy", "num_threads":2},
            "limits":{
                "max_text_code_points":500,
                "max_audio_bytes":16_777_216,
                "synthesis_timeout_ms":30_000
            },
            "accepted_audio_deliveries":[
                {"mode":"incremental", "format":"audio/pcm;codec=pcm_s16le"}
            ]
        }),
    );
    let initialized = provider.control();
    assert_eq!(initialized["result"]["ready"], true, "{initialized}");
    assert!(initialized["result"]["utterance_options_schema"]["properties"]["speed"].is_null());

    provider.request(
        "synth",
        "synthesis.start",
        json!({
            "text":"Cancel this deliberately long protocol synthesis after the first delivered XN frame so acknowledgement ordering and post-acknowledgement silence are both exercised.",
            "audio_delivery":{"mode":"incremental", "format":"audio/pcm;codec=pcm_s16le"},
            "utterance_options":{"seed":42}
        }),
    );
    let mut heard_audio = false;
    while !heard_audio {
        let (kind, payload) = read_frame(&mut provider.stdout);
        if kind == AUDIO {
            assert!(!payload.is_empty());
            heard_audio = true;
        } else {
            let control: Value = serde_json::from_slice(&payload).unwrap();
            assert_eq!(control["event"], "synthesis.audio_begin");
        }
    }
    provider.request("cancel", "synthesis.cancel", json!({"request_id":"synth"}));
    let mut acknowledged = false;
    let mut terminal = false;
    while !terminal {
        let (kind, payload) = read_frame(&mut provider.stdout);
        if kind == AUDIO {
            assert!(
                !acknowledged,
                "audio was emitted after cancellation acknowledgement"
            );
            continue;
        }
        let control: Value = serde_json::from_slice(&payload).unwrap();
        if control["id"] == "cancel" {
            assert_eq!(control["result"]["accepted"], true);
            acknowledged = true;
        } else {
            assert_eq!(control["id"], "synth");
            assert_eq!(control["error"]["code"], "cancelled");
            assert!(acknowledged);
            terminal = true;
        }
    }
    provider.request("health", "runtime.health", json!({}));
    assert_eq!(provider.control()["result"]["status"], "ready");
    provider.shutdown();
}

fn protocol_management_smoke(
    storage: &tempfile::TempDir,
    model_id: &str,
    reference_path: &std::path::Path,
) {
    let mut provider = ProviderProcess::spawn();
    provider.request(
        "hello",
        "protocol.hello",
        json!({
            "protocol":"utterpipe.tts", "versions":[1],
            "expected_provider":"pocket-tts", "session":"management",
            "utterance_schema_profiles":["utterpipe.utterance-options/1"],
            "host":{"name":"xn-management-test", "version":"0.1.0"}
        }),
    );
    assert_eq!(provider.control()["result"]["version"], 1);
    provider.request(
        "init",
        "session.initialize",
        json!({
            "data_dir":storage.path().join("data").to_string_lossy(),
            "cache_dir":storage.path().join("cache").to_string_lossy(),
            "provider_options":{"model":model_id, "voice":"management-caro", "num_threads":2}
        }),
    );
    assert_eq!(provider.control()["result"]["ready"], true);
    provider.request("validate-missing", "provider.validate", json!({}));
    assert_eq!(provider.control()["result"]["status"], "incomplete");
    provider.request(
        "import",
        "asset.import",
        json!({
            "kind":"voice",
            "source_path":reference_path.to_string_lossy(),
            "requested_id":"management-caro",
            "consent_confirmed":true,
            "operation_id":"xn-import-1"
        }),
    );
    let imported = provider.control();
    assert_eq!(imported["result"]["status"], "installed", "{imported}");
    provider.request("validate-ready", "provider.validate", json!({}));
    assert_eq!(provider.control()["result"]["status"], "ready");
    provider.request(
        "models",
        "catalog.items",
        json!({"catalog_id":"models", "scope":"installed", "refresh":false, "limit":100}),
    );
    let models = provider.control();
    assert!(
        models["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model["id"] == model_id)
    );
    provider.request(
        "plan",
        "prepare.plan",
        json!({"refresh":false, "allow_network":false}),
    );
    let plan = provider.control();
    assert!(plan["result"]["actions"].as_array().unwrap().is_empty());
    provider.request(
        "apply",
        "prepare.apply",
        json!({
            "plan_id":plan["result"]["plan_id"],
            "accepted_licenses":[],
            "operation_id":"xn-prepare-1"
        }),
    );
    assert_eq!(provider.control()["result"]["status"], "ready");
    provider.shutdown();
}
