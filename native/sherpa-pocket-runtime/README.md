# sherpa-pocket-runtime

This unpublished workspace dependency is the smallest safe Rust surface needed
by `utterpipe-pocket-tts`. It mirrors the pinned sherpa-onnx 1.13.4 Pocket TTS C
ABI, verifies explicitly supplied native archives, and links only Pocket's
runtime dependencies.

The upstream multi-engine bundle contains eSpeak NG even though Pocket has no
phonemizer. This binding deliberately omits that GPL archive. Three unreachable
eSpeak references retained by sherpa's generic factory resolve to fail-closed
Apache-2.0 shims; CI and release builds remove the archive before linking and
exercise a real Pocket model afterward.

This crate is internal, not a general sherpa-onnx binding and not published to
crates.io. ABI structs and symbols must be re-audited before changing the pinned
sherpa-onnx version.
