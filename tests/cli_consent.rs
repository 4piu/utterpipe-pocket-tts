use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

fn run_with_piped_yes(arguments: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_utterpipe-pocket-tts"))
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"yes\nyes\nyes\nyes\n")
        .unwrap();
    child.wait_with_output().unwrap()
}

fn storage_arguments<'a>(data: &'a Path, cache: &'a Path) -> [String; 4] {
    [
        "--data-dir".to_owned(),
        data.display().to_string(),
        "--cache-dir".to_owned(),
        cache.display().to_string(),
    ]
}

#[test]
fn piped_yes_does_not_authorize_model_preparation() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let cache = temp.path().join("cache");
    let storage = storage_arguments(&data, &cache);
    let arguments = [
        "models",
        "prepare",
        storage[0].as_str(),
        storage[1].as_str(),
        storage[2].as_str(),
        storage[3].as_str(),
    ];

    let output = run_with_piped_yes(&arguments);

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("preparation requires --yes")
    );
    assert!(!data.exists());
    assert!(!cache.exists());
}

#[test]
fn piped_yes_does_not_authorize_voice_import() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let cache = temp.path().join("cache");
    let source = temp.path().join("reference.wav");
    let storage = storage_arguments(&data, &cache);
    let arguments = [
        "voices",
        "import",
        source.to_str().unwrap(),
        "--id",
        "test-voice",
        storage[0].as_str(),
        storage[1].as_str(),
        storage[2].as_str(),
        storage[3].as_str(),
    ];

    let output = run_with_piped_yes(&arguments);

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("requires --consent-confirmed")
    );
    assert!(!data.exists());
    assert!(!cache.exists());
}
