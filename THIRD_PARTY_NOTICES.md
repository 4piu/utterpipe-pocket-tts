# Third-party notices

The provider is Apache-2.0. Its exact release dependency inventory, copyright
notices, and license texts are in
[`THIRD_PARTY_LICENSES.html`](THIRD_PARTY_LICENSES.html); `Cargo.lock` and each
release SBOM record exact versions and sources.

## Native Pocket runtime

The executable incorporates these pinned components but no model weights or
reference recordings:

- `ptts` 0.2.2 from the project fork of
  [`LaurentMazare/xn-ptts`](https://github.com/4piu/xn-ptts), revision
  `4dbd8d6832cf4e093d08a1bd4666a08783345e7b`, MIT OR Apache-2.0. The fork adds
  learned voice-BOS and asymmetric Mimi bottleneck support required by the
  evaluated April checkpoint.
- [`xn`](https://github.com/LaurentMazare/xn) 0.1.21, MIT OR Apache-2.0,
  including its GGML CPU runtime.
- [`sentencepiece`](https://github.com/danieldk/sentencepiece) 0.13.2,
  MIT OR Apache-2.0, and `sentencepiece-sys` 0.13.2, Apache-2.0.

The old sherpa-onnx/ONNX Runtime adapter and converted ONNX model catalog are
not part of the active source, dependency graph, or release executable.

## Separately acquired models and voices

The quick-start model is fetched only after explicit user authorization and is
stored separately from the provider executable:

- Pocket TTS source and model card:
  <https://huggingface.co/kyutai/pocket-tts>
- Pocket TTS source code: <https://github.com/kyutai-labs/pocket-tts>
- pinned model revision:
  `19f95fe2df36e79fbd9f10008595cc4c977a0fcc`
- model license: Creative Commons Attribution 4.0
- additional publisher prohibited-use conditions: the conditions displayed on
  the pinned model page

The provider requires separate acceptance of `cc-by-4.0` and
`pocket-tts-acceptable-use` before download, local quantization, and
installation. Those prompts help surface the upstream conditions; they are not
legal advice and do not replace the user's obligations.

Optional curated reference voices come from
[`kyutai/tts-voices`](https://huggingface.co/kyutai/tts-voices). The embedded
catalog pins each exact revision, file hash, license, and attribution. It
includes Voice-Zero recordings identified upstream as CC0, Alba MacKenna and
VCTK recordings under CC BY 4.0, and Expresso recordings under non-commercial
CC BY-NC 4.0. The provider displays the applicable terms before downloading.

Arbitrary imported recordings remain user-provided assets. The provider grants
no rights to a recording, speaker identity, or generated voice.

## Regenerating the Rust license report

Use cargo-about 0.9.1:

```sh
cargo about generate --locked --offline --fail --all-features \
  about.hbs --output-file THIRD_PARTY_LICENSES.generated.html
tr -d '\r' < THIRD_PARTY_LICENSES.generated.html > THIRD_PARTY_LICENSES.normalized.html
cmp THIRD_PARTY_LICENSES.html THIRD_PARTY_LICENSES.normalized.html
```
