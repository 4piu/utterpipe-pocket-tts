use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::Command,
    thread,
};

#[cfg(unix)]
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use serde_json::Value;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use utterpipe_pocket_tts::{
    store::Store,
    voice::{CURATED_VOICES, ExpectedDownload, VoiceDownloadError, download_voice},
};

fn reference_wav(junk_bytes: usize) -> Vec<u8> {
    let samples = vec![200_i16; 16_000];
    let data_bytes = u32::try_from(samples.len() * 2).unwrap();
    let junk_bytes = u32::try_from(junk_bytes).unwrap();
    let padding = junk_bytes & 1;
    let riff_bytes = 36_u32 + data_bytes + 8 + junk_bytes + padding;
    let mut bytes = Vec::with_capacity(riff_bytes as usize + 8);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_bytes.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&16_000_u32.to_le_bytes());
    bytes.extend_from_slice(&32_000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"JUNK");
    bytes.extend_from_slice(&junk_bytes.to_le_bytes());
    bytes.resize(bytes.len() + junk_bytes as usize + padding as usize, 0);
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn serve_once(body: Vec<u8>) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
    });
    (format!("http://{address}/voice.wav"), handle)
}

#[cfg(unix)]
fn serve_slow(body: Vec<u8>) -> (String, mpsc::Receiver<()>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted_sender, accepted_receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        if write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .is_err()
        {
            return;
        }
        accepted_sender.send(()).unwrap();
        for chunk in body.chunks(16 * 1_024) {
            if stream.write_all(chunk).is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
    });
    (
        format!("http://{address}/slow-voice.wav"),
        accepted_receiver,
        handle,
    )
}

fn storage_args(data: &Path, cache: &Path) -> [String; 4] {
    [
        "--data-dir".to_owned(),
        data.display().to_string(),
        "--cache-dir".to_owned(),
        cache.display().to_string(),
    ]
}

fn import_url(temp: &TempDir, url: &str, id: &str) -> std::process::Output {
    let data = temp.path().join("data");
    let cache = temp.path().join("cache");
    let storage = storage_args(&data, &cache);
    Command::new(env!("CARGO_BIN_EXE_utterpipe-pocket-tts"))
        .args(["voices", "import", url, "--id", id, "--consent-confirmed"])
        .args(storage)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .output()
        .unwrap()
}

#[test]
fn explicit_http_source_with_credentials_uses_the_regular_importer() {
    let temp = TempDir::new().unwrap();
    let (url, server) = serve_once(reference_wav(16));
    let url = url.replacen("http://", "http://user:password@", 1);

    let output = import_url(&temp, &url, "from-url");
    server.join().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("installed: voice:from-url")
    );
    let store = Store::new(temp.path().join("data"), temp.path().join("cache")).unwrap();
    assert_eq!(store.voice_catalog().unwrap()[0]["id"], "from-url");
    let staged: Vec<_> = std::fs::read_dir(temp.path().join("cache/tmp"))
        .unwrap()
        .collect();
    assert!(staged.is_empty());
}

#[test]
fn large_http_source_warns_but_imports_and_cleans_staging() {
    let temp = TempDir::new().unwrap();
    let (url, server) = serve_once(reference_wav(5 * 1_024 * 1_024 + 1));

    let output = import_url(&temp, &url, "large-url");
    server.join().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("warning: voice source is unusually large")
    );
    let staged: Vec<_> = std::fs::read_dir(temp.path().join("cache/tmp"))
        .unwrap()
        .collect();
    assert!(staged.is_empty());
}

#[test]
fn relative_local_source_remains_a_file_import() {
    let temp = TempDir::new().unwrap();
    std::fs::write(temp.path().join("reference.wav"), reference_wav(0)).unwrap();
    let data = temp.path().join("data");
    let cache = temp.path().join("cache");
    let storage = storage_args(&data, &cache);

    let output = Command::new(env!("CARGO_BIN_EXE_utterpipe-pocket-tts"))
        .args([
            "voices",
            "import",
            "reference.wav",
            "--id",
            "relative-file",
            "--consent-confirmed",
        ])
        .args(storage)
        .current_dir(temp.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let store = Store::new(data, cache).unwrap();
    assert_eq!(store.voice_catalog().unwrap()[0]["id"], "relative-file");
}

#[test]
fn invalid_download_is_rejected_without_leaving_staging_or_a_voice() {
    let temp = TempDir::new().unwrap();
    let (url, server) = serve_once(b"not a wave".to_vec());

    let output = import_url(&temp, &url, "invalid-url");
    server.join().unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("reference audio is invalid")
    );
    let staged: Vec<_> = std::fs::read_dir(temp.path().join("cache/tmp"))
        .unwrap()
        .collect();
    assert!(staged.is_empty());
    let store = Store::new(temp.path().join("data"), temp.path().join("cache")).unwrap();
    assert!(store.voice_catalog().unwrap().is_empty());
}

#[test]
fn http_import_honors_proxy_environment() {
    let temp = TempDir::new().unwrap();
    let (proxy, server) = serve_once(reference_wav(0));
    let data = temp.path().join("data");
    let cache = temp.path().join("cache");
    let storage = storage_args(&data, &cache);

    let output = Command::new(env!("CARGO_BIN_EXE_utterpipe-pocket-tts"))
        .args([
            "voices",
            "import",
            "http://voice-source.invalid/reference.wav",
            "--id",
            "through-proxy",
            "--consent-confirmed",
        ])
        .args(storage)
        .env("HTTP_PROXY", &proxy)
        .env("http_proxy", &proxy)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .output()
        .unwrap();
    server.join().unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let store = Store::new(data, cache).unwrap();
    assert_eq!(store.voice_catalog().unwrap()[0]["id"], "through-proxy");
}

#[cfg(unix)]
#[test]
fn sigint_cancels_a_slow_download_and_cleans_staging() {
    let temp = TempDir::new().unwrap();
    let (url, accepted, server) = serve_slow(reference_wav(8 * 1_024 * 1_024));
    let data = temp.path().join("data");
    let cache = temp.path().join("cache");
    let storage = storage_args(&data, &cache);
    let mut child = Command::new(env!("CARGO_BIN_EXE_utterpipe-pocket-tts"))
        .args([
            "voices",
            "import",
            &url,
            "--id",
            "cancelled-url",
            "--consent-confirmed",
        ])
        .args(storage)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    accepted.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("voice import did not respond to SIGINT");
        }
        thread::sleep(Duration::from_millis(20));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    server.join().unwrap();

    assert!(!status.success());
    assert!(stderr.contains("voice download was cancelled"), "{stderr}");
    let staged: Vec<_> = std::fs::read_dir(cache.join("tmp")).unwrap().collect();
    assert!(staged.is_empty());
}

#[tokio::test]
async fn pinned_download_checksum_failure_removes_private_staging() {
    let temp = TempDir::new().unwrap();
    let body = reference_wav(0);
    let bytes = body.len() as u64;
    let (url, server) = serve_once(body);

    let error = download_voice(
        url.parse().unwrap(),
        temp.path(),
        Some(ExpectedDownload {
            bytes,
            sha256: "0000000000000000000000000000000000000000000000000000000000000000",
        }),
        CancellationToken::new(),
        |_| {},
    )
    .await
    .err()
    .unwrap();
    server.join().unwrap();

    assert!(matches!(error, VoiceDownloadError::Integrity));
    let staged: Vec<_> = std::fs::read_dir(temp.path().join("tmp"))
        .unwrap()
        .collect();
    assert!(staged.is_empty());
}

#[test]
fn available_catalog_is_offline_pinned_and_reports_storage_status() {
    let temp = TempDir::new().unwrap();
    let storage = storage_args(&temp.path().join("data"), &temp.path().join("cache"));

    let output = Command::new(env!("CARGO_BIN_EXE_utterpipe-pocket-tts"))
        .args(["voices", "available"])
        .args(storage)
        .output()
        .unwrap();

    assert!(output.status.success());
    let descriptors: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(descriptors.len(), CURATED_VOICES.len());
    assert!(
        descriptors
            .iter()
            .all(|voice| voice["status"] == "available")
    );
    assert!(descriptors.iter().all(|voice| {
        voice["revision"]
            .as_str()
            .is_some_and(|revision| revision.len() == 40)
    }));
}
