# UtterPipe Pocket TTS provider

A local, offline-after-setup [UtterPipe](../utterpipe/SPEC.md) provider for
[Pocket TTS](https://github.com/kyutai-labs/pocket-tts). It is one small
standalone executable; model weights and user-approved reference voices are
installed separately in host-supplied data/cache directories.

The provider supports complete PCM16 WAV and genuine incremental 24 kHz mono
PCM16 output. It keeps one warm sherpa-onnx engine per runtime process and can
serve repeated sequential utterances without Python or a child service.

## Status

Version 0.1 pins one converted int8 model,
`pocket-tts-int8-2026-01-26`, and statically links sherpa-onnx 1.13.4. macOS
arm64, Linux x86_64, and Windows x86_64 are verified locally. A release target
is publishable only after the same native inference, conformance, and dependency
checks pass in release CI.

The converted model archive discloses CC-BY-4.0 terms, upstream acceptable-use
conditions, and an explicit non-commercial notice. The preparation command
shows and requires all three acknowledgements. See
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and [SPEC.md](SPEC.md).

## Install and discovery

Download the executable for your platform and place it next to the host or on
`PATH` as `utterpipe-pocket-tts`. Agent Speak discovers providers by executable
name; there is no registry or provider-specific configuration file. The host's
single `config.toml` selects the provider, model, imported voice ID, and options.

The model and imported voice are not bundled. Prepare the model once, then
import a reference WAV for which you have the necessary rights and consent:

```text
utterpipe-pocket-tts models prepare \
  --data-dir /absolute/provider-data --cache-dir /absolute/provider-cache \
  --accept pocket-tts-cc-by-4.0 \
  --accept pocket-tts-acceptable-use \
  --accept pocket-tts-converted-artifact-non-commercial --yes

utterpipe-pocket-tts voices import /absolute/reference.wav --id my-voice \
  --consent-confirmed \
  --data-dir /absolute/provider-data --cache-dir /absolute/provider-cache
```

Reference audio must be a regular mono PCM16 RIFF/WAVE file at 16–48 kHz,
between 1 and 30 seconds, and no larger than 5 MiB.

## Commands

```text
utterpipe-pocket-tts info
utterpipe-pocket-tts doctor --data-dir <path> --cache-dir <path>
utterpipe-pocket-tts models list --data-dir <path> --cache-dir <path>
utterpipe-pocket-tts models prepare [--archive <path>] --accept <id>... --yes \
  --data-dir <path> --cache-dir <path>
utterpipe-pocket-tts models remove <model-id> --yes \
  --data-dir <path> --cache-dir <path>
utterpipe-pocket-tts voices list --data-dir <path> --cache-dir <path>
utterpipe-pocket-tts voices import <wav> --id <id> --consent-confirmed \
  --data-dir <path> --cache-dir <path>
utterpipe-pocket-tts voices remove <voice-id> --yes \
  --data-dir <path> --cache-dir <path>
utterpipe-pocket-tts protocol --stdio
```

Every mutating CLI path uses the same checked store operations and cross-process
locks as the framed management protocol. The CLI renders its own equivalent
human-readable plan; protocol plan IDs remain scoped to their live management
session. Removal never proceeds while a runtime leases an affected model or
voice.

## Provider options

All options are fixed at runtime initialization. Defaults are shown below.

```toml
num_threads = 2
speed = 1.0
seed = 42
max_reference_audio_seconds = 10.0
voice_embedding_cache_capacity = 16
```

See [SPEC.md](SPEC.md) for ranges, exact wire contracts, model hashes, storage
layout, network policy, and error mapping.

## Build

Rust 1.88 or newer is required. The crate deliberately enables sherpa-onnx's
`static` feature. Supply a pre-fetched native archive directory to Cargo so a
release build never depends on an implicit build-time download:

```text
SHERPA_ONNX_ARCHIVE_DIR=/absolute/sherpa-onnx-prebuilt cargo build --locked --release
```

The verified sherpa native archives are:

| Target | Archive | SHA-256 |
| --- | --- | --- |
| macOS arm64 | `sherpa-onnx-v1.13.4-osx-arm64-static-lib.tar.bz2` | `57801db2bbb786a5d343f515a38ff210b401842338bdc804fa075312d1cd2404` |
| Linux x86_64 | `sherpa-onnx-v1.13.4-linux-x64-static-lib.tar.bz2` | `98b0e31996426f6e78244dbce1955548f2c64e8f01c4be75b85af7cdaa2e8d5c` |
| Windows x86_64 | `sherpa-onnx-v1.13.4-win-x64-static-MT-Release-lib.tar.bz2` | `d81bd1d25112540862d2387072e76b2b6843ef962918d6b5c7db5a19c6276b4c` |

The repository configures a static MSVC CRT for Windows x86_64 so it matches
the pinned `MT` archive and does not require a separately installed VC runtime.

Run the ordinary and opt-in real-model tests with:

```text
SHERPA_ONNX_ARCHIVE_DIR=/absolute/sherpa-onnx-prebuilt cargo test --locked --all-targets

UTTERPIPE_POCKET_MODEL_ARCHIVE=/absolute/pinned-model.tar.bz2 \
UTTERPIPE_POCKET_REFERENCE_WAV=/absolute/reference.wav \
SHERPA_ONNX_ARCHIVE_DIR=/absolute/sherpa-onnx-prebuilt \
  cargo test --locked --test real_model -- --ignored --nocapture
```

## Cancellation boundary

Downloads, hashing, extraction, imports, and removals use cooperative
cancellation checkpoints and recoverable staging. Native inference observes
cancellation through the sherpa callback. If foreign code fails to return
within the two-second grace period, the provider exits without waiting for the
blocking pool; the host may also force-terminate it under the UtterPipe process
policy. Atomic active pointers and OS-released leases keep installed state
recoverable after either path.

## Voice provenance and consent

An imported reference remains the user's asset. The provider records explicit
consent confirmation and content provenance but does not decide whether the
recording, speaker identity, or generated voice may lawfully be used. It grants
no rights to them, never publishes imported voices, and never treats model test
WAVs as available voice packs. Users and downstream applications must obtain
and honor all necessary permissions, privacy rights, publicity rights, and
applicable acceptable-use rules.

The provider stores a normalized local WAV plus hashes and technical metadata;
it does not retain the original source path.

## License

The provider adapter is Apache-2.0. Model weights and user voice assets are
separate and retain their own terms.
