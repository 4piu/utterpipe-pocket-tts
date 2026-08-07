use serde_json::{Value, json};

pub const MODEL_ID: &str = "pocket-tts-int8-2026-01-26";
pub const MODEL_NAME: &str = "Pocket TTS int8 (sherpa-onnx conversion, 2026-01-26)";
pub const ARCHIVE_NAME: &str = "sherpa-onnx-pocket-tts-int8-2026-01-26.tar.bz2";
pub const ARCHIVE_SHA256: &str = "2f3b88823cbbb9bf0b2477ec8ae7b3fec417b3a87b6bb5f256dba66f2ad967cb";
pub const MACOS_ARM64_NATIVE_ARCHIVE_SHA256: &str =
    "57801db2bbb786a5d343f515a38ff210b401842338bdc804fa075312d1cd2404";
pub const MACOS_X64_NATIVE_ARCHIVE_SHA256: &str =
    "2bda2c10b31a1cfc45d9f9e14bd4983743ec3779d309e42d99a6c8fa1689043f";
pub const LINUX_X64_NATIVE_ARCHIVE_SHA256: &str =
    "98b0e31996426f6e78244dbce1955548f2c64e8f01c4be75b85af7cdaa2e8d5c";
pub const WINDOWS_X64_NATIVE_ARCHIVE_SHA256: &str =
    "d81bd1d25112540862d2387072e76b2b6843ef962918d6b5c7db5a19c6276b4c";
pub const ARCHIVE_BYTES: u64 = 98_336_520;
pub const INSTALLED_BYTES: u64 = 198_310_873;
pub const SOURCE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/sherpa-onnx-pocket-tts-int8-2026-01-26.tar.bz2";
pub const VERSION_TOKEN: &str = ARCHIVE_SHA256;

pub const REQUIRED_FILES: &[(&str, &str)] = &[
    (
        "decoder.int8.onnx",
        "12b0857402d31aead94df19d6783b4350d1f740e811f3a3202c70ad89ae11eea",
    ),
    (
        "encoder.onnx",
        "e8f2f6d301ffb96e398b138a7dc6d3038622d236044636b73d920bab85890260",
    ),
    (
        "lm_flow.int8.onnx",
        "8d627d235c44a597da908e1085ebe241cbbe358964c502c5a5063d18851a5529",
    ),
    (
        "lm_main.int8.onnx",
        "bfc0c7e7e3d72864fa3bb2ee499f62f21ddc1474b885f5f3ca570f8be73e787e",
    ),
    (
        "text_conditioner.onnx",
        "0b84e837d7bfaf2c896627b03e3f080320309f37f4fc7df7698c644f7ba5e6b1",
    ),
    (
        "token_scores.json",
        "5be2f278caf9b9800741f0fd82bff677f4943ec764c356f907213434b622d958",
    ),
    (
        "vocab.json",
        "6fb646346cf931016f70c4921aab0900ce7a304b893cb02135c74e294abfea01",
    ),
    (
        "README.md",
        "2d05e627fe4fa3c625e822efcd20ad2ca62eb3b4fc1d67ae2625cb106b4f689c",
    ),
    (
        "LICENSE",
        "fe7b4ce83b8381cc5b216bbb4af73c570688d1b819c73bbaed8ca401f4677cd6",
    ),
];

pub const LICENSE_IDS: &[&str] = &[
    "pocket-tts-cc-by-4.0",
    "pocket-tts-acceptable-use",
    "pocket-tts-converted-artifact-non-commercial",
];

#[must_use]
pub fn licenses() -> Value {
    json!([
        {
            "id": LICENSE_IDS[0], "name": "Pocket TTS converted-model attribution (CC-BY-4.0)",
            "url": "https://creativecommons.org/licenses/by/4.0/", "requires_acceptance": true
        },
        {
            "id": LICENSE_IDS[1], "name": "Pocket TTS acceptable-use conditions",
            "url": "https://huggingface.co/kyutai/pocket-tts", "requires_acceptance": true
        },
        {
            "id": LICENSE_IDS[2], "name": "Converted artifact non-commercial notice",
            "url": SOURCE_URL, "requires_acceptance": true
        }
    ])
}

#[must_use]
pub fn model_descriptor(status: &str) -> Value {
    json!({
        "id": MODEL_ID,
        "name": MODEL_NAME,
        "version": "2026-01-26",
        "status": status,
        "languages": ["en"],
        "download_bytes": ARCHIVE_BYTES,
        "installed_bytes": INSTALLED_BYTES,
        "license": {
            "id": "multiple-required-disclosures",
            "url": "https://huggingface.co/kyutai/pocket-tts",
            "requires_acceptance": true
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_archive_checksum_is_consistent_in_release_docs() {
        for checksum in [
            MACOS_ARM64_NATIVE_ARCHIVE_SHA256,
            MACOS_X64_NATIVE_ARCHIVE_SHA256,
            LINUX_X64_NATIVE_ARCHIVE_SHA256,
            WINDOWS_X64_NATIVE_ARCHIVE_SHA256,
        ] {
            assert!(include_str!("../README.md").contains(checksum));
            assert!(include_str!("../docs/SPEC.md").contains(checksum));
        }
    }
}
