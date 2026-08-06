use std::{
    io::{Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde_json::{Value, json};
use tempfile::TempDir;
use utterpipe_pocket_tts::{
    model::{LICENSE_IDS, MODEL_ID},
    store::Store,
};

const CONTROL: u8 = 0x01;

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
            "kind": "request", "id": id, "method": method, "params": params
        }))
        .unwrap();
        write_frame(&mut self.stdin, CONTROL, &payload);
    }

    fn response(&mut self) -> Value {
        let (kind, payload) = read_frame(&mut self.stdout);
        assert_eq!(kind, CONTROL);
        serde_json::from_slice(&payload).unwrap()
    }

    fn hello(&mut self, session: &str) {
        self.request(
            "hello",
            "protocol.hello",
            json!({
                "protocol": "utterpipe.tts",
                "versions": [1],
                "expected_provider": "pocket-tts",
                "session": session,
                "host": {"name": "provider-test", "version": "0.1.0"}
            }),
        );
        let response = self.response();
        assert_eq!(response["result"]["provider"]["slug"], "pocket-tts");
    }

    fn initialize_management(&mut self, temp: &TempDir) {
        self.request("init", "session.initialize", initialize_params(temp));
        assert_eq!(self.response()["result"]["ready"], true);
    }

    fn shutdown(mut self) {
        self.request("shutdown", "session.shutdown", json!({}));
        assert_eq!(self.response()["result"]["accepted"], true);
        drop(self.stdin);
        assert!(self.child.wait().unwrap().success());
    }
}

fn initialize_params(temp: &TempDir) -> Value {
    json!({
        "data_dir": temp.path().join("data").to_string_lossy(),
        "cache_dir": temp.path().join("cache").to_string_lossy(),
        "options": {},
        "selection": {"model_id": MODEL_ID, "voice_id": "test-voice"},
        "limits": {
            "max_text_code_points": 4096,
            "max_audio_bytes": 16_777_216,
            "synthesis_timeout_ms": 30_000
        },
        "accepted_delivery_modes": ["incremental", "complete"],
        "accepted_audio_formats": [
            "audio/pcm;codec=pcm_s16le", "audio/wav;codec=pcm_s16le"
        ]
    })
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

fn write_reference(path: &std::path::Path, junk_bytes: usize) {
    let samples = vec![200_i16; 16_000];
    let data_bytes = u32::try_from(samples.len() * 2).unwrap();
    let junk_bytes = u32::try_from(junk_bytes).unwrap();
    let riff_bytes = 36_u32
        .checked_add(data_bytes)
        .and_then(|value| value.checked_add(8 + junk_bytes))
        .unwrap();
    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(b"RIFF").unwrap();
    file.write_all(&riff_bytes.to_le_bytes()).unwrap();
    file.write_all(b"WAVEfmt ").unwrap();
    file.write_all(&16_u32.to_le_bytes()).unwrap();
    file.write_all(&1_u16.to_le_bytes()).unwrap();
    file.write_all(&1_u16.to_le_bytes()).unwrap();
    file.write_all(&16_000_u32.to_le_bytes()).unwrap();
    file.write_all(&32_000_u32.to_le_bytes()).unwrap();
    file.write_all(&2_u16.to_le_bytes()).unwrap();
    file.write_all(&16_u16.to_le_bytes()).unwrap();
    file.write_all(b"JUNK").unwrap();
    file.write_all(&junk_bytes.to_le_bytes()).unwrap();
    file.write_all(&vec![0_u8; junk_bytes as usize]).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&data_bytes.to_le_bytes()).unwrap();
    for sample in samples {
        file.write_all(&sample.to_le_bytes()).unwrap();
    }
}

#[test]
fn direct_cli_flushes_output_when_stdout_is_piped() {
    let output = Command::new(env!("CARGO_BIN_EXE_utterpipe-pocket-tts"))
        .arg("info")
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Pocket TTS provider"));
    assert!(stdout.contains("protocol: utterpipe.tts v1"));
}

#[test]
fn inspect_rejects_known_cross_session_methods_and_unknown_methods_exactly() {
    let temp = TempDir::new().unwrap();
    let mut provider = ProviderProcess::spawn();
    provider.hello("inspect");

    provider.request("init", "session.initialize", initialize_params(&temp));
    assert_eq!(provider.response()["error"]["code"], "wrong_session");
    provider.request("health", "runtime.health", json!({}));
    assert_eq!(provider.response()["error"]["code"], "wrong_session");
    provider.request("catalog", "catalog.voices", json!({}));
    assert_eq!(provider.response()["error"]["code"], "wrong_session");
    provider.request("unknown", "future.method", json!({}));
    assert_eq!(provider.response()["error"]["code"], "method_not_supported");
    provider.shutdown();
}

#[test]
fn management_catalog_import_plan_and_remove_are_wire_compatible() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("reference.wav");
    write_reference(&source, 16);
    let mut provider = ProviderProcess::spawn();
    provider.hello("management");
    provider.initialize_management(&temp);

    provider.request(
        "voices-empty",
        "catalog.voices",
        json!({"model_id": MODEL_ID, "scope": "installed", "refresh": false}),
    );
    assert_eq!(provider.response()["result"]["voices"], json!([]));
    provider.request(
        "models-installed",
        "catalog.models",
        json!({"scope": "installed", "refresh": false}),
    );
    assert_eq!(provider.response()["result"]["models"], json!([]));

    provider.request(
        "plan",
        "prepare.plan",
        json!({"refresh": false, "allow_network": false}),
    );
    let plan = provider.response();
    assert_eq!(plan["result"]["licenses"].as_array().unwrap().len(), 3);
    let plan_id = plan["result"]["plan_id"].as_str().unwrap();
    provider.request(
        "apply-without-terms",
        "prepare.apply",
        json!({"plan_id": plan_id, "accepted_licenses": [], "operation_id": "apply-1"}),
    );
    assert_eq!(provider.response()["error"]["code"], "license_required");

    provider.request(
        "import",
        "voice.import",
        json!({
            "source_path": source.to_string_lossy(), "requested_id": "test-voice",
            "consent_confirmed": true, "operation_id": "import-1"
        }),
    );
    assert_eq!(provider.response()["result"]["status"], "installed");
    provider.request(
        "voices",
        "catalog.voices",
        json!({"model_id": MODEL_ID, "scope": "installed", "refresh": false}),
    );
    let voices = provider.response();
    assert_eq!(voices["result"]["voices"][0]["id"], "test-voice");
    assert_eq!(
        voices["result"]["voices"][0]["license"]["url"],
        "https://github.com/4piu/utterpipe-pocket-tts#voice-provenance-and-consent"
    );
    provider.request(
        "voices-available",
        "catalog.voices",
        json!({"model_id": MODEL_ID, "scope": "available", "refresh": false}),
    );
    assert_eq!(provider.response()["result"]["voices"], json!([]));

    provider.request(
        "remove-plan",
        "remove.plan",
        json!({"artifacts": ["voice:test-voice"], "purge": false}),
    );
    let remove_plan = provider.response();
    let remove_plan_id = remove_plan["result"]["plan_id"].as_str().unwrap();
    provider.request(
        "remove",
        "remove.apply",
        json!({"plan_id": remove_plan_id, "operation_id": "remove-1"}),
    );
    assert_eq!(
        provider.response()["result"]["removed"],
        json!(["voice:test-voice"])
    );
    provider.shutdown();
}

#[test]
fn competing_process_mutation_returns_resource_busy_before_source_decode() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path().join("data"), temp.path().join("cache")).unwrap();
    let _guard = store.begin_mutation().unwrap();
    let mut provider = ProviderProcess::spawn();
    provider.hello("management");
    provider.initialize_management(&temp);
    provider.request(
        "import",
        "voice.import",
        json!({
            "source_path": temp.path().join("missing.wav").to_string_lossy(),
            "requested_id": "test-voice", "consent_confirmed": true,
            "operation_id": "contended-import"
        }),
    );
    assert_eq!(provider.response()["error"]["code"], "resource_busy");
    provider.shutdown();
}

#[test]
fn contended_prepare_can_be_retried_and_detects_a_stale_plan() {
    let temp = TempDir::new().unwrap();
    let store = Store::new(temp.path().join("data"), temp.path().join("cache")).unwrap();
    let mut provider = ProviderProcess::spawn();
    provider.hello("management");
    provider.initialize_management(&temp);
    provider.request(
        "plan",
        "prepare.plan",
        json!({"refresh": false, "allow_network": true}),
    );
    let planned = provider.response();
    let plan_id = planned["result"]["plan_id"].as_str().unwrap().to_owned();
    let accepted = LICENSE_IDS.to_vec();

    let guard = store.begin_mutation().unwrap();
    provider.request(
        "contended",
        "prepare.apply",
        json!({
            "plan_id": plan_id, "accepted_licenses": accepted,
            "operation_id": "contended-prepare"
        }),
    );
    assert_eq!(provider.response()["error"]["code"], "resource_busy");

    let model_root = temp.path().join("data/models").join(MODEL_ID);
    std::fs::create_dir_all(&model_root).unwrap();
    std::fs::write(model_root.join("active"), "corrupt-version\n").unwrap();
    drop(guard);
    provider.request(
        "stale",
        "prepare.apply",
        json!({
            "plan_id": plan_id, "accepted_licenses": LICENSE_IDS,
            "operation_id": "stale-prepare"
        }),
    );
    assert_eq!(provider.response()["error"]["code"], "plan_stale");
    provider.shutdown();
}

#[test]
fn shutdown_cancels_an_active_import_and_exits_cleanly() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("large-reference.wav");
    write_reference(&source, 5_000_000);
    let mut provider = ProviderProcess::spawn();
    provider.hello("management");
    provider.initialize_management(&temp);
    provider.request(
        "import",
        "voice.import",
        json!({
            "source_path": source.to_string_lossy(), "requested_id": "test-voice",
            "consent_confirmed": true, "operation_id": "import-cancel"
        }),
    );
    provider.request("shutdown", "session.shutdown", json!({}));
    let terminal = provider.response();
    assert_eq!(terminal["id"], "import");
    assert_eq!(terminal["error"]["code"], "cancelled");
    let shutdown = provider.response();
    assert_eq!(shutdown["id"], "shutdown");
    assert_eq!(shutdown["result"]["accepted"], true);
    drop(provider.stdin);
    assert!(provider.child.wait().unwrap().success());
    assert!(
        Store::new(temp.path().join("data"), temp.path().join("cache"))
            .unwrap()
            .voice_catalog()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn shutdown_terminal_matches_whether_import_cancel_or_commit_won() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("reference.wav");
    write_reference(&source, 0);
    let mut provider = ProviderProcess::spawn();
    provider.hello("management");
    provider.initialize_management(&temp);
    provider.request(
        "import",
        "voice.import",
        json!({
            "source_path": source.to_string_lossy(), "requested_id": "race-voice",
            "consent_confirmed": true, "operation_id": "import-race"
        }),
    );
    provider.request("shutdown", "session.shutdown", json!({}));
    let terminal = provider.response();
    assert_eq!(terminal["id"], "import");
    let reported_installed = terminal["result"]["status"] == "installed";
    if !reported_installed {
        assert_eq!(terminal["error"]["code"], "cancelled");
    }
    assert_eq!(provider.response()["result"]["accepted"], true);
    drop(provider.stdin);
    assert!(provider.child.wait().unwrap().success());
    let installed = Store::new(temp.path().join("data"), temp.path().join("cache"))
        .unwrap()
        .voice_catalog()
        .unwrap()
        .iter()
        .any(|voice| voice["id"] == "race-voice");
    assert_eq!(reported_installed, installed);
}

#[test]
fn eof_during_an_active_import_exits_without_publishing_partial_state() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("large-reference.wav");
    write_reference(&source, 5_000_000);
    let mut provider = ProviderProcess::spawn();
    provider.hello("management");
    provider.initialize_management(&temp);
    provider.request(
        "import",
        "voice.import",
        json!({
            "source_path": source.to_string_lossy(), "requested_id": "test-voice",
            "consent_confirmed": true, "operation_id": "import-eof"
        }),
    );
    drop(provider.stdin);
    assert!(provider.child.wait().unwrap().success());
    assert!(
        Store::new(temp.path().join("data"), temp.path().join("cache"))
            .unwrap()
            .voice_catalog()
            .unwrap()
            .is_empty()
    );
}
