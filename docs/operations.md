# Setup and operations reference

This page contains the less frequently needed operational details for
`utterpipe-pocket-tts`. Start with the [README quick start](../README.md) if you
only want to install the provider and make it speak through Agent Speak.

## Install, update, and uninstall

The release installers verify archive checksums and install the executable for
the current user:

```sh
curl -fsSL https://raw.githubusercontent.com/4piu/utterpipe-pocket-tts/main/install.sh | sh
```

```powershell
irm https://raw.githubusercontent.com/4piu/utterpipe-pocket-tts/main/install.ps1 | iex
```

Run the same command again to verify and replace the executable with the latest
release. Installed models and imported voices are left untouched. Stop running
provider instances first, especially on Windows where an active executable may
be locked.

Remove only the executable with:

```sh
curl -fsSL https://raw.githubusercontent.com/4piu/utterpipe-pocket-tts/main/install.sh | sh -s -- --uninstall
```

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/4piu/utterpipe-pocket-tts/main/install.ps1))) -Uninstall
```

Model and imported voice assets are preserved by default. Add `--purge` or
`-Purge` only to irreversibly delete them during uninstall.

Agent Speak discovers `utterpipe-pocket-tts` first beside its own executable,
then on `PATH`; there is no provider registry or provider-specific config file.
Other UtterPipe hosts define their own executable discovery behavior.

## Interactive direct commands

For a person using a terminal, the shortest setup is:

```text
utterpipe-pocket-tts models prepare
utterpipe-pocket-tts voices import /absolute/reference.wav --id my-voice
utterpipe-pocket-tts doctor
```

Preparation prints all model terms, prompts for each required acknowledgement,
and confirms the download. Voice import prompts for rights and consent. Every
prompt defaults to no.

Interactive authorization is available only when stdin, stdout, and stderr are
terminals. Redirected or piped commands never treat input as authorization.
Automation must pass every applicable flag explicitly, as described below.

Other inspection and removal commands are:

```text
utterpipe-pocket-tts info
utterpipe-pocket-tts doctor [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts models list [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts models remove <model-id> [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts voices list [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts voices remove <voice-id> [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts protocol --stdio
```

Removal is also confirmed interactively. Every mutating CLI path uses the same
checked store operations and cross-process locks as the framed management
protocol. Removal never proceeds while a runtime leases an affected model or
voice.

## Non-interactive automation

Non-interactive model preparation must explicitly accept all three disclosures
and confirm the operation:

```text
utterpipe-pocket-tts models prepare \
  --accept pocket-tts-cc-by-4.0 \
  --accept pocket-tts-acceptable-use \
  --accept pocket-tts-converted-artifact-non-commercial \
  --yes
```

An already-downloaded pinned archive may be supplied with
`--archive /absolute/model-archive.tar.bz2`. Automated voice import and removal
likewise require explicit confirmation:

```text
utterpipe-pocket-tts voices import /absolute/reference.wav --id my-voice \
  --consent-confirmed
utterpipe-pocket-tts models remove pocket-tts-int8-2026-01-26 --yes
utterpipe-pocket-tts voices remove my-voice --yes
```

Agent Speak can perform the equivalent generic provider operations. Its
non-interactive preparation syntax is:

```text
agent-speak prepare --config /absolute/agent-speak.toml \
  --accept-license pocket-tts-cc-by-4.0 \
  --accept-license pocket-tts-acceptable-use \
  --accept-license pocket-tts-converted-artifact-non-commercial \
  --yes
```

Agent Speak asset import requires an absolute source path:

```text
agent-speak provider import --config /absolute/agent-speak.toml --kind voice \
  --source /absolute/reference.wav --id my-voice --consent-confirmed
```

## Storage and custom hosts

When `--data-dir` and `--cache-dir` are omitted, human-facing direct commands
use the same platform-standard provider roots as Agent Speak:

| Platform | Data | Cache |
| --- | --- | --- |
| macOS | `~/Library/Application Support/UtterPipe/providers/pocket-tts/data` | `~/Library/Caches/UtterPipe/providers/pocket-tts` |
| Linux | `${XDG_DATA_HOME:-~/.local/share}/utterpipe/providers/pocket-tts` | `${XDG_CACHE_HOME:-~/.cache}/utterpipe/providers/pocket-tts` |
| Windows | `%LOCALAPPDATA%\UtterPipe\providers\pocket-tts\data` | `%LOCALAPPDATA%\UtterPipe\providers\pocket-tts\cache` |

Each flag independently overrides its default. A custom host should pass both
explicitly and reuse those exact roots for setup and serving:

```text
utterpipe-pocket-tts models prepare \
  --data-dir /absolute/provider-data --cache-dir /absolute/provider-cache
utterpipe-pocket-tts voices import /absolute/reference.wav --id my-voice \
  --data-dir /absolute/provider-data --cache-dir /absolute/provider-cache
```

The direct defaults are a CLI convenience only. `protocol --stdio` never
discovers storage from the provider environment: its host must supply absolute,
private `data_dir` and `cache_dir` values during session initialization. The
framed protocol never prompts; management authorization remains explicit in
its plan/apply, license-acceptance, and consent fields.

## Voice storage, provenance, and consent

An imported reference remains the user's asset. The provider records explicit
consent confirmation and a content hash but does not decide whether the
recording, speaker identity, or generated voice may lawfully be used. It grants
no rights to them, never publishes imported voices, and never treats model test
WAVs as voice packs. Users and downstream applications must obtain and honor
all necessary permissions, privacy rights, publicity rights, and applicable
acceptable-use rules.

The provider stores a normalized local WAV plus hashes and technical metadata;
it does not retain the original source path.

## Build and test

Rust 1.88 or newer is required. The repository's Pocket-only runtime binding
links pinned sherpa-onnx native libraries without the unrelated eSpeak NG
archive in upstream's multi-engine bundle. Supply a pre-fetched native archive
directory so release builds never depend on an implicit build-time download:

```text
SHERPA_ONNX_ARCHIVE_DIR=/absolute/sherpa-onnx-prebuilt cargo build --locked --release
```

Verified sherpa native archives:

| Target | Archive | SHA-256 |
| --- | --- | --- |
| macOS arm64 | `sherpa-onnx-v1.13.4-osx-arm64-static-lib.tar.bz2` | `57801db2bbb786a5d343f515a38ff210b401842338bdc804fa075312d1cd2404` |
| macOS x86_64 | `sherpa-onnx-v1.13.4-osx-x64-static-lib.tar.bz2` | `2bda2c10b31a1cfc45d9f9e14bd4983743ec3779d309e42d99a6c8fa1689043f` |
| Linux x86_64 | `sherpa-onnx-v1.13.4-linux-x64-static-lib.tar.bz2` | `98b0e31996426f6e78244dbce1955548f2c64e8f01c4be75b85af7cdaa2e8d5c` |
| Windows x86_64 | `sherpa-onnx-v1.13.4-win-x64-static-MT-Release-lib.tar.bz2` | `d81bd1d25112540862d2387072e76b2b6843ef962918d6b5c7db5a19c6276b4c` |

The repository configures a static MSVC CRT for Windows x86_64 to match the
pinned `MT` archive, so a separate VC runtime installation is not required.

Run ordinary and opt-in real-model tests with:

```text
SHERPA_ONNX_ARCHIVE_DIR=/absolute/sherpa-onnx-prebuilt cargo test --locked --all-targets

UTTERPIPE_POCKET_MODEL_ARCHIVE=/absolute/pinned-model.tar.bz2 \
SHERPA_ONNX_ARCHIVE_DIR=/absolute/sherpa-onnx-prebuilt \
  cargo test --locked --test real_model -- --ignored --nocapture
```

The native test generates a deterministic synthetic reference by default. Set
`UTTERPIPE_POCKET_REFERENCE_WAV` to an absolute user-approved reference only
for an optional listening test.

## Runtime and cancellation boundaries

The provider supports complete PCM16 WAV and genuine incremental 24 kHz mono
PCM16 output. It keeps one warm sherpa-onnx engine per runtime process and
serves repeated sequential utterances without Python or a child service.

Downloads, hashing, extraction, imports, and removals use cooperative
cancellation checkpoints and recoverable staging. Native inference observes
cancellation through the sherpa callback. If foreign code does not return
within the two-second grace period, the provider exits without waiting for the
blocking pool; the host may also force-terminate it under the UtterPipe process
policy. Atomic active pointers and OS-released leases keep installed state
recoverable after either path.

Tagged releases contain the provider executable and documentation, not the
model or a voice. Each archive has a SHA-256 checksum; corresponding source and
a CycloneDX SBOM are separate release assets. See the
[third-party notices](../THIRD_PARTY_NOTICES.md) and
[release-integrity status](release-integrity.md).
