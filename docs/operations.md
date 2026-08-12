# Setup and operations reference

Start with the [README quick start](../README.md) if you only want to make the
provider speak through Agent Speak.

## Install, update, and uninstall

The release installers verify archive checksums and install the executable for
the current user:

```sh
curl -fsSL https://raw.githubusercontent.com/4piu/utterpipe-pocket-tts/main/install.sh | sh
```

```powershell
irm https://raw.githubusercontent.com/4piu/utterpipe-pocket-tts/main/install.ps1 | iex
```

Run the same command to update. Models and voices are preserved. Stop provider
processes first, especially on Windows where an active executable may be
locked. Uninstall the executable with `--uninstall` on Unix or `-Uninstall` in
PowerShell. Add `--purge`/`-Purge` only when you intend to irreversibly remove
provider data and cache too.

Agent Speak discovers the provider beside its own executable and then on
`PATH`. Other UtterPipe hosts define their own discovery behavior.

## Direct commands

The shortest interactive setup is:

```text
hf auth login
utterpipe-pocket-tts models prepare
utterpipe-pocket-tts voices install
utterpipe-pocket-tts doctor
```

All prompts default to no and appear only when stdin, stdout, and stderr are
terminals. Redirected input is never authorization. Inspection and management
commands include:

```text
utterpipe-pocket-tts info
utterpipe-pocket-tts doctor [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts models list [--data-dir <path>] [--cache-dir <path>]
utterpipe-pocket-tts models prepare [--source-dir <path>] [--accept <id>]... [--yes] ...
utterpipe-pocket-tts models import <bundle-directory> [--accept <id>]... [--yes] ...
utterpipe-pocket-tts models remove <model-id> [--yes] ...
utterpipe-pocket-tts voices list ...
utterpipe-pocket-tts voices available ...
utterpipe-pocket-tts voices install [selection]... [--accept <id>]... [--yes] ...
utterpipe-pocket-tts voices import <path-or-url> --id <id> [--consent-confirmed] ...
utterpipe-pocket-tts voices remove <voice-id> [--yes] ...
utterpipe-pocket-tts protocol --stdio
```

The voice catalog is embedded and listing it is offline. Interactive terminals
show eight numbered items per page and accept multiple numbers, IDs,
comma-separated selections, and ranges. Redirected output is JSON Lines with a
stable `number` field. Downloads honor the system proxy and normal proxy
environment variables.

`models import` is the expert/offline route. A bundle must contain
`manifest.json`, `model.gguf`, `config.json`, and exactly one supported
`tokenizer.model` or `tokenizer.json`. The provider verifies bounded metadata,
runtime compatibility, exact file hashes, tokenizer/config semantics, and a
native model load before atomic activation. It never treats an imported bundle
as a catalog compatibility claim.

## Model authentication and preparation

The quick-start model is gated by its upstream publisher. First accept the
access agreement at <https://huggingface.co/kyutai/pocket-tts>. The provider
resolves authentication in this order:

1. `HF_TOKEN`;
2. the file named by `HF_TOKEN_PATH`;
3. `${HF_HOME}/token`;
4. `${XDG_CACHE_HOME}/huggingface/token`;
5. `~/.cache/huggingface/token`.

The token is bounded, used only as an HTTPS bearer credential to the pinned
Hugging Face repository, and never included in output or diagnostics. Source
downloads use the system proxy, have exact size/SHA-256 requirements, and are
cached by content hash. Q8 conversion runs locally on CPU and remains
cancellable between tensors.

For an offline/reproducibility setup, place the exact pinned
`model.safetensors` and `tokenizer.model` directly in one directory and use:

```text
utterpipe-pocket-tts models prepare --source-dir /absolute/source-directory
```

The provider still verifies both files and performs the same conversion. It
does not accept an arbitrary model through this path.

The verified source cache is about 219 MB; installed model files are about
148 MB. After a successful installation, the cache directory may be deleted
while no preparation operation is running. It will be recreated on demand.

## Non-interactive automation

Automation must explicitly accept the two pinned model disclosures and confirm
the operation:

```text
utterpipe-pocket-tts models prepare \
  --accept cc-by-4.0 \
  --accept pocket-tts-acceptable-use \
  --yes
```

Equivalent Agent Speak preparation is:

```text
agent-speak prepare --config /absolute/agent-speak.toml \
  --accept-license cc-by-4.0 \
  --accept-license pocket-tts-acceptable-use \
  --yes
```

Voice operations require their displayed catalog license or explicit import
consent, for example:

```text
utterpipe-pocket-tts voices install voice-zero-caro-davy \
  --accept cc0-1.0 --yes
utterpipe-pocket-tts voices import /absolute/reference.wav --id my-voice \
  --consent-confirmed
utterpipe-pocket-tts models remove pocket-tts-english-2026-04-q8 --yes
utterpipe-pocket-tts voices remove my-voice --yes
```

Agent Speak asset import requires an absolute local source path:

```text
agent-speak provider import --config /absolute/agent-speak.toml --kind voice \
  --source /absolute/reference.wav --id my-voice --consent-confirmed
```

## Storage and custom hosts

Human-facing commands default to the same provider roots as Agent Speak:

| Platform | Data | Cache |
| --- | --- | --- |
| macOS | `~/Library/Application Support/UtterPipe/providers/pocket-tts/data` | `~/Library/Caches/UtterPipe/providers/pocket-tts` |
| Linux | `${XDG_DATA_HOME:-~/.local/share}/utterpipe/providers/pocket-tts` | `${XDG_CACHE_HOME:-~/.cache}/utterpipe/providers/pocket-tts` |
| Windows | `%LOCALAPPDATA%\UtterPipe\providers\pocket-tts\data` | `%LOCALAPPDATA%\UtterPipe\providers\pocket-tts\cache` |

Each override is independent. A custom host should pass both roots consistently
to setup and runtime operations. `protocol --stdio` never discovers roots from
the environment: the host supplies absolute private paths during session
initialization. The framed protocol never prompts.

Every mutation uses private staging, atomic active pointers, integrity checks,
and a provider-wide cross-process lease. Model, voice, and derived voice-state
files also remain leased while a runtime uses them, so removal fails safely
instead of invalidating an active engine.

## Voice input and provenance

The importer accepts local files and explicit HTTP(S) URLs. WAV input has no
arbitrary total-byte ceiling: metadata is streamed with bounded memory and a
warning appears above 5 MiB. Accepted decoded audio remains bounded to mono
PCM16, 16–48 kHz, and 1–30 seconds. Temporary downloads are private and removed
after success, failure, timeout, or cancellation.

The store keeps a normalized WAV, hashes, technical metadata, and consent. It
does not retain an arbitrary import URL/path. Curated voices additionally keep
their pinned repository, revision, license, and attribution. Users and hosts
remain responsible for permission, privacy, publicity, and acceptable-use
requirements.

## Build and test

Rust 1.88 or newer and a platform C/C++ toolchain are required. The XN runtime,
GGML backend, and SentencePiece tokenizer build from pinned Rust/git dependency
revisions; no sherpa/ONNX archive or Python environment is involved.

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
```

The ignored real-model test uses an already prepared self-describing bundle and
an authorized reference WAV:

```sh
UTTERPIPE_POCKET_XN_BUNDLE=/absolute/bundle \
UTTERPIPE_POCKET_XN_REFERENCE=/absolute/reference.wav \
  cargo test --locked --test xn_real_model -- --ignored --nocapture
```

For repeatable provider-process latency, RTF, memory, artifact-size, and
cancellation measurements, use `utterpipe-benchmark` as documented in the
[runtime evaluation](runtime-evaluation.md).

## Runtime and cancellation

The provider serves complete PCM16 WAV and genuine incremental 24 kHz mono
PCM16 from one warm in-process XN engine. Synthesis has bounded output,
deadline, and queue behavior. Cancellation is checked during generation and no
audio is emitted after the cancellation acknowledgement. If foreign work ever
fails to return within the protocol grace period, the host may force-terminate
the process; atomic storage and OS-released leases keep data recoverable.

Tagged release archives contain the executable and documentation, never a
model or voice. See [third-party notices](../THIRD_PARTY_NOTICES.md) and
[release-integrity status](release-integrity.md).
