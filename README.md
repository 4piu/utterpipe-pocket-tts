# UtterPipe Pocket TTS provider

A local neural-text-to-speech
[UtterPipe](https://github.com/4piu/utterpipe/blob/main/docs/SPEC.md) provider
for [Pocket TTS](https://github.com/kyutai-labs/pocket-tts). It runs as one
standalone executable, stays offline after setup, and provides complete WAV or
incremental 24 kHz mono PCM16 audio. Agent Speak keeps one warm engine process
for responsive repeated speech.

## Before you install

The provider executable **does not bundle the model or a voice**:

- `models prepare` downloads the pinned converted model after showing its
  CC-BY-4.0 terms, acceptable-use conditions, and non-commercial notice.
- Voices are not downloaded automatically. The provider offers a small,
  checksum-pinned selection from Kyutai's official
  [`kyutai/tts-voices`](https://huggingface.co/kyutai/tts-voices) repository,
  or you can import another local or HTTP(S) WAV that you have permission to
  use. The catalog includes CC0, CC BY 4.0, and non-commercial CC BY-NC 4.0
  collections; every selection retains its own license and attribution.
- The provider stores the model and imported reference locally. It does not
  upload or publish imported voices.

The current model is English-only. The model determines which language can be
spoken; a reference WAV supplies the speaker, accent, and style but does not add
a language to the model. For best results, match the reference recording to the
model language. Current upstream Pocket TTS offers separate language-specific
models, while this provider supports only the pinned English sherpa-onnx
conversion. Plan on an approximately 98 MB download, 198 MB of installed model
files, one interactive setup, and a suitable reference recording before
expecting synthesis to work.

## Quick start with Agent Speak

These steps use the provider's platform-standard storage roots, which are also
used by Agent Speak.

### 1. Install the provider

macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/4piu/utterpipe-pocket-tts/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/4piu/utterpipe-pocket-tts/main/install.ps1 | iex
```

Open a new terminal if the installer added the executable directory to `PATH`.
Confirm that `utterpipe-pocket-tts --version` works before continuing.

If you use the Agent Speak VS Code extension, run **Agent Speak: Open Provider
Folder** and copy the installed executable there. The extension keeps providers
isolated from the `PATH` inherited by the VS Code desktop process; model and
voice setup below still uses the same platform-standard storage. Use
`command -v utterpipe-pocket-tts` on macOS/Linux or
`(Get-Command utterpipe-pocket-tts).Source` in PowerShell to locate it.

### 2. Download and prepare the model

Run this in a terminal without redirecting or piping its input or output:

```sh
utterpipe-pocket-tts models prepare
```

The command displays the model disclosures and asks before accepting each one
and downloading the pinned model. Every prompt defaults to no.

### 3. Install a voice

The provider does not bundle voices. Open its interactive, paged catalog and
choose one or more numbered selections:

```sh
utterpipe-pocket-tts voices install
```

`voices available` shows the same numbered pages without installing. You can
also select IDs, numbers, comma lists, or ranges directly:

```sh
utterpipe-pocket-tts voices available
utterpipe-pocket-tts voices install 2 5-7
utterpipe-pocket-tts voices install voice-zero-caro-davy
```

The install command shows each exact source revision, attribution, checksum,
and applicable license, then asks for acknowledgement before downloading. To
use another authorized recording instead, reuse the same import command for a
path or URL:

```sh
utterpipe-pocket-tts voices import ./reference.wav --id my-voice
utterpipe-pocket-tts voices import https://example.test/reference.wav --id my-voice
```

Import asks you to confirm the necessary rights and consent. The WAV must be a
regular mono PCM16 RIFF/WAVE file at 16–48 kHz and 1–30 seconds long. Inputs
larger than 5 MiB produce a warning but are streamed and remain cancellable.
For best results, use a clean recording with one speaker and little background
noise.

### 4. Configure Agent Speak

Download the maintained
[complete Pocket TTS profile](https://github.com/4piu/agent-speak/blob/master/examples/pocket-provider.toml):

```sh
curl -fsSLo agent-speak.toml https://raw.githubusercontent.com/4piu/agent-speak/master/examples/pocket-provider.toml
```

On PowerShell, use:

```powershell
irm https://raw.githubusercontent.com/4piu/agent-speak/master/examples/pocket-provider.toml -OutFile agent-speak.toml
```

The template selects the pinned model and `voice = "my-voice"`. Change the
`voice` value to your chosen ID—for example `voice-zero-caro-davy`—then validate
the complete profile and installed assets:

```sh
agent-speak validate --config /absolute/path/to/agent-speak.toml
```

Configure your MCP client to launch:

```text
agent-speak serve --config /absolute/path/to/agent-speak.toml
```

Reload the MCP server, call `get_audio_capabilities`, then ask the agent:

> Say “Pocket TTS is working.”

`validate` checks the provider, model, and voice but does not synthesize audio;
the MCP `speak_text` call is the final listening test. See
[Agent Speak's MCP setup](https://github.com/4piu/agent-speak#register-with-an-mcp-host)
for client-specific configuration.

## Provider options

Agent Speak's complete profile uses these fixed engine settings:

```toml
[tts.provider_options]
model = "pocket-tts-int8-2026-01-26"
voice = "my-voice"
num_threads = 2
max_reference_audio_seconds = 10.0
voice_embedding_cache_capacity = 16
```

The inexpensive per-request options are `speed` (default `1.0`) and `seed`
(default `42`). A host may send configured defaults on every synthesis and let
an agent override authorized values for one request; neither option rebuilds
the engine.

## Status and compatibility

Version 0.1 pins `pocket-tts-int8-2026-01-26` and statically links sherpa-onnx
1.13.4. macOS arm64, Linux x86_64, and Windows x86_64 are verified locally.
Tagged release archives contain the provider and documentation, not the model
or a voice. Kyutai publishes the upstream gated
[`model.safetensors` weights](https://huggingface.co/kyutai/pocket-tts); they
are provenance/source weights for compatible Pocket runtimes, not drop-in files
for this provider's current sherpa-onnx model installer.

## More documentation

- [Setup, direct CLI, automation, storage, and development](docs/operations.md)
- [Provider and wire specification](docs/SPEC.md)
- [Third-party terms and notices](THIRD_PARTY_NOTICES.md)
- [Release-integrity status](docs/release-integrity.md)

An imported reference remains your asset. You are responsible for permission,
privacy, publicity, and acceptable-use requirements for both the recording and
generated voice. The provider grants no rights to either.

## License

The provider adapter is Apache-2.0. Model weights and user voice assets are
separate and retain their own terms.
