# Third-party notices

The provider is licensed under Apache-2.0. Its exact Rust dependency inventory,
copyright notices, and license texts for all release targets are in
[`THIRD_PARTY_LICENSES.html`](THIRD_PARTY_LICENSES.html). This includes the
ISC/Apache/MIT/BSD notices incorporated by AWS-LC and the bzip2 notice. The
provider reads each platform's native certificate store, so the current
runtime graph does not distribute the separately licensed `webpki-roots` data.
`Cargo.lock` and each release SBOM record the corresponding versions and
checksums.

## Native Pocket runtime

The executable statically incorporates these pinned native components:

- sherpa-onnx 1.13.4, Copyright (c) 2022-2026 Next-gen Kaldi contributors,
  Apache-2.0,
  <https://github.com/k2-fsa/sherpa-onnx/tree/v1.13.4>;
- ONNX Runtime 1.27.0, MIT, including its complete upstream
  [`native/ONNXRUNTIME_THIRD_PARTY_NOTICES.txt`](native/ONNXRUNTIME_THIRD_PARTY_NOTICES.txt);
- kaldi-decoder 0.3.0, kaldifst, kaldi-native-fbank 1.22.3, OpenFST
  1.8.5-2026-04-11, and simple-sentencepiece 0.7, Apache-2.0;
- kissfft at `febd4ca`, BSD-3-Clause; and
- piper-phonemize at `f3ff95a`, MIT.

The upstream sherpa static archive also contains a GPL eSpeak NG library that
Pocket TTS neither needs nor uses. The local `sherpa-pocket-runtime` binding
does not issue a link directive for that archive. Three references retained by
sherpa's generic multi-engine factory resolve to fail-closed Apache-2.0 shims,
so no eSpeak implementation or data enters the distributed executable. Release
testing links and runs the real Pocket model with both `libespeak-ng` and its
`libucd` Unicode-data companion absent.

The provider binary and source tree do not contain Pocket TTS model weights or
reference recordings. The optional pinned converted model is downloaded and
stored separately. Its archive identifies these sources and conditions:

- Pocket TTS: <https://github.com/kyutai-labs/pocket-tts>
- Model card and acceptable-use conditions: <https://huggingface.co/kyutai/pocket-tts>
- Optional reference-voice repository: <https://huggingface.co/kyutai/tts-voices>
  (recording licenses and attribution requirements vary by collection)
- ONNX conversion: <https://github.com/KevinAHM/pocket-tts-onnx-export>
- Converted archive license: CC-BY-4.0
- Converted archive README notice: “It is for non-commercial.”

Because those disclosures are not equivalent, this provider requires separate
acceptance of `pocket-tts-cc-by-4.0`, `pocket-tts-acceptable-use`, and
`pocket-tts-converted-artifact-non-commercial` before installation and applies
the most restrictive disclosed interpretation. This is a conservative product
policy, not legal advice.

Imported voice references remain user-provided assets. The provider does not
grant rights to a recording, speaker identity, or generated voice.

Regenerate and compare the Rust report with cargo-about 0.9.1:

```sh
cargo about generate --locked --offline --fail --all-features \
  about.hbs --output-file THIRD_PARTY_LICENSES.generated.html
tr -d '\r' < THIRD_PARTY_LICENSES.generated.html > THIRD_PARTY_LICENSES.normalized.html
cmp THIRD_PARTY_LICENSES.html THIRD_PARTY_LICENSES.normalized.html
```
