# UtterPipe Pocket TTS provider

An offline neural-text-to-speech
[UtterPipe](https://github.com/4piu/utterpipe/blob/main/docs/SPEC.md) provider
for [Kyutai Pocket TTS](https://github.com/kyutai-labs/pocket-tts). It uses the
native [XN Pocket TTS runtime](https://github.com/4piu/xn-ptts), supports
complete WAV and incremental 24 kHz mono PCM16 audio, and keeps one warm engine
for responsive repeated speech.

## Before you install

The executable bundles **neither a model nor a voice**.

- The quick start downloads the gated official April 2026 English Pocket TTS
  model from [`kyutai/pocket-tts`](https://huggingface.co/kyutai/pocket-tts),
  verifies the pinned source, and converts it locally to the tested XN Q8
  profile. Accept the model-page access agreement and authenticate with Hugging
  Face first.
- The source download is about 219 MB and the installed runtime model is about
  148 MB. The verified source cache can be removed after installation.
- A reference WAV supplies the speaker, accent, and style. It does not add a
  language to the English model. The provider offers a checksum-pinned voice
  catalog from [`kyutai/tts-voices`](https://huggingface.co/kyutai/tts-voices)
  or can import an authorized local/HTTP(S) WAV.
- Model terms, acceptable-use conditions, and each voice's license remain in
  force. The provider records consent and provenance but grants no rights to a
  recording, speaker identity, or generated voice.

## Quick start

### 1. Install the provider

macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/4piu/utterpipe-pocket-tts/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/4piu/utterpipe-pocket-tts/main/install.ps1 | iex
```

If you use the Agent Speak VS Code extension, run **Agent Speak: Open Provider
Folder** and copy the installed executable there. The extension deliberately
keeps its providers isolated from ordinary `PATH` discovery.

### 2. Authenticate and prepare the model

On the [official model page](https://huggingface.co/kyutai/pocket-tts), sign in
and accept the access agreement. Then authenticate the local Hugging Face CLI:

```sh
hf auth login
```

Alternatively set `HF_TOKEN` or `HF_TOKEN_PATH`. The provider never prints the
credential. Run the interactive preparation command:

```sh
utterpipe-pocket-tts models prepare
```

It displays the pinned source and both required disclosures, defaults every
prompt to no, downloads with the system proxy, verifies exact hashes, performs
the deterministic CPU Q8 conversion, and atomically installs the result.

### 3. Install a voice

Open the paged catalog and choose one or more numbered entries:

```sh
utterpipe-pocket-tts voices install
```

You can also list the catalog or select numbers and ranges directly:

```sh
utterpipe-pocket-tts voices available
utterpipe-pocket-tts voices install 2 5-7
```

The command shows exact provenance, checksum, attribution, and license before
download. To use another recording, reuse the importer for a path or URL:

```sh
utterpipe-pocket-tts voices import ./reference.wav --id my-voice
utterpipe-pocket-tts voices import https://example.test/reference.wav --id my-voice
```

The input must be mono PCM16 RIFF/WAVE at 16–48 kHz and 1–30 seconds. A clean
single-speaker English recording usually works best. The imported WAV is
normalized locally and may then be discarded if you do not need your original.

## Agent Speak configuration

Download the maintained complete profile:

```sh
curl -fsSLo agent-speak.toml https://raw.githubusercontent.com/4piu/agent-speak/master/examples/pocket-provider.toml
```

Set its `voice` to your installed ID, then validate and serve it:

```sh
agent-speak validate --config /absolute/path/to/agent-speak.toml
agent-speak serve --config /absolute/path/to/agent-speak.toml
```

Reload the MCP server, call `get_audio_capabilities`, and ask the agent to say:

> Pocket TTS is working.

Validation checks the provider, model, and voice; `speak_text` is the final
audible test.

## Provider options

```toml
[tts.provider_options]
model = "pocket-tts-english-2026-04-q8"
voice = "my-voice"
num_threads = 4
```

`num_threads` accepts `1..64`; omission defaults to 4 on arm64 and 8 elsewhere.
The only per-utterance option is `seed` (default `42`). The active XN backend
does not expose a speed control.

## Model profile and compatibility

The catalog deliberately contains one known-good bootstrap profile:

- official source revision
  `19f95fe2df36e79fbd9f10008595cc4c977a0fcc`;
- forked XN runtime revision
  `4dbd8d6832cf4e093d08a1bd4666a08783345e7b`;
- local deterministic Q8 conversion and a provider-pinned acoustic profile;
- locally verified on macOS arm64, Linux x86_64, and Windows x86_64.

Other model formats are not implied compatible. `models import <directory>` is
the expert/offline route for a complete self-describing XN bundle containing
`manifest.json`, `model.gguf`, `config.json`, and exactly one supported
`tokenizer.model` or `tokenizer.json`.

## More documentation

- [Setup, automation, storage, and development](docs/operations.md)
- [Provider specification](docs/SPEC.md)
- [Runtime evaluation and retained evidence](docs/runtime-evaluation.md)
- [Third-party terms and notices](THIRD_PARTY_NOTICES.md)
- [Release-integrity status](docs/release-integrity.md)

## License

The provider adapter is Apache-2.0. Model weights and voice assets are separate
and retain their own terms.
