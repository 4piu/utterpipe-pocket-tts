# UtterPipe Pocket TTS provider

A local, offline-after-setup
[UtterPipe](https://github.com/4piu/utterpipe/blob/main/docs/SPEC.md) provider
for [Pocket TTS](https://github.com/kyutai-labs/pocket-tts). It is one small
standalone executable; model weights and user-approved reference voices are
installed separately in host-supplied data/cache directories.
The launching UtterPipe host supplies storage roots and session options.

The provider supports complete PCM16 WAV and genuine incremental 24 kHz mono
PCM16 output. It keeps one warm sherpa-onnx engine per runtime process and can
serve repeated sequential utterances without Python or a child service.

## Status

Version 0.1 pins one converted int8 model,
`pocket-tts-int8-2026-01-26`, and statically links sherpa-onnx 1.13.4. macOS
arm64, Linux x86_64, and Windows x86_64 are verified locally. Release CI must
pass real native inference, cancellation, shared-lease, packaging, and
dependency checks before publishing any target.

The converted model archive discloses CC-BY-4.0 terms, upstream acceptable-use
conditions, and an explicit non-commercial notice. The preparation command
shows and requires all three acknowledgements. See
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and the
[provider specification](docs/SPEC.md).

## Install

Download the executable for your platform as `utterpipe-pocket-tts`. Agent
Speak discovers it beside the `agent-speak` executable or on `PATH`; other
UtterPipe hosts define their own discovery behavior. There is no registry or
provider-specific configuration file.

After the first tagged release, the repository scripts provide
checksum-verifying per-user installation:

```sh
curl -fsSL https://raw.githubusercontent.com/4piu/utterpipe-pocket-tts/main/install.sh | sh
```

```powershell
irm https://raw.githubusercontent.com/4piu/utterpipe-pocket-tts/main/install.ps1 | iex
```

The same one-line command handles initial installation, reinstallation, and
updates. Run it again to verify and replace the executable with the current
latest release; installed models and imported voices are left untouched. Stop
running provider instances first, especially on Windows where an active
executable may be locked.

Remove the executable while preserving models and imported voices with:

```sh
curl -fsSL https://raw.githubusercontent.com/4piu/utterpipe-pocket-tts/main/install.sh | sh -s -- --uninstall
```

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/4piu/utterpipe-pocket-tts/main/install.ps1))) -Uninstall
```

Model and imported voice assets are preserved by default. Add `--purge` or
`-Purge` only to irreversibly delete them during uninstall.

## Use with Agent Speak

Select the provider, model, and imported voice in Agent Speak's only config
file:

```toml
schema_version = 1

[tts]
enabled = true
backend = "utterpipe-pocket-tts"
maximum_characters = 500
agent_utterance_options = ["speed", "seed"]

[tts.provider_options]
model = "pocket-tts-int8-2026-01-26"
voice = "my-voice"
num_threads = 2
speed = 1.0
seed = 42
```

The model and voice are not bundled. Use Agent Speak's management commands so
preparation and serving receive the same platform-default storage roots:

```text
agent-speak prepare --config ./agent-speak.toml \
  --accept-license pocket-tts-cc-by-4.0 \
  --accept-license pocket-tts-acceptable-use \
  --accept-license pocket-tts-converted-artifact-non-commercial --yes

agent-speak provider import --config ./agent-speak.toml --kind voice \
  --source /absolute/reference.wav --id my-voice --consent-confirmed
```

Reference audio must be a regular mono PCM16 RIFF/WAVE file at 16–48 kHz,
between 1 and 30 seconds, and no larger than 5 MiB.

## Direct provider commands

Custom-host integrators and provider developers can manage explicit roots
directly. The `--data-dir` and `--cache-dir` values must exactly match the roots
that the host will later pass to the provider:

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

`model` and `voice` are required runtime selections. The engine controls have
the defaults shown below.

```toml
model = "pocket-tts-int8-2026-01-26"
voice = "my-voice"
num_threads = 2
speed = 1.0
seed = 42
max_reference_audio_seconds = 10.0
voice_embedding_cache_capacity = 16
```

The host may pass `speed` and `seed` for one synthesis request when the user
allows those agent-controlled options. Per-request values override the fixed
defaults without changing the config file. Other controls remain startup-fixed.

See [the provider specification](docs/SPEC.md) for ranges, exact wire contracts,
model hashes, storage
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
| macOS x86_64 | `sherpa-onnx-v1.13.4-osx-x64-static-lib.tar.bz2` | `2bda2c10b31a1cfc45d9f9e14bd4983743ec3779d309e42d99a6c8fa1689043f` |
| Linux x86_64 | `sherpa-onnx-v1.13.4-linux-x64-static-lib.tar.bz2` | `98b0e31996426f6e78244dbce1955548f2c64e8f01c4be75b85af7cdaa2e8d5c` |
| Windows x86_64 | `sherpa-onnx-v1.13.4-win-x64-static-MT-Release-lib.tar.bz2` | `d81bd1d25112540862d2387072e76b2b6843ef962918d6b5c7db5a19c6276b4c` |

The repository configures a static MSVC CRT for Windows x86_64 so it matches
the pinned `MT` archive and does not require a separately installed VC runtime.

Run the ordinary and opt-in real-model tests with:

```text
SHERPA_ONNX_ARCHIVE_DIR=/absolute/sherpa-onnx-prebuilt cargo test --locked --all-targets

UTTERPIPE_POCKET_MODEL_ARCHIVE=/absolute/pinned-model.tar.bz2 \
SHERPA_ONNX_ARCHIVE_DIR=/absolute/sherpa-onnx-prebuilt \
  cargo test --locked --test real_model -- --ignored --nocapture
```

The native test generates a deterministic synthetic reference by default so CI
does not depend on a person's voice. Set `UTTERPIPE_POCKET_REFERENCE_WAV` to an
absolute user-approved reference when performing an optional listening test.

Tagged release archives contain the provider executable and its documentation,
not the model or a voice. Each archive has a SHA-256 checksum; corresponding
source and a CycloneDX SBOM are separate release assets. Artifacts are unsigned
unless their release notes state otherwise.

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
