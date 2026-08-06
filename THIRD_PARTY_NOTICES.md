# Third-party notices

The provider binary statically links sherpa-onnx 1.13.4 and its native runtime.
sherpa-onnx is Copyright (c) 2022-2026 Next-gen Kaldi contributors and is
licensed under Apache-2.0: <https://github.com/k2-fsa/sherpa-onnx>.

The provider binary and source tree do not contain Pocket TTS model weights or
reference recordings. The optional pinned converted model is downloaded and
stored separately. Its archive identifies these sources and conditions:

- Pocket TTS: <https://github.com/kyutai-labs/pocket-tts>
- Model card and acceptable-use conditions: <https://huggingface.co/kyutai/pocket-tts>
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
