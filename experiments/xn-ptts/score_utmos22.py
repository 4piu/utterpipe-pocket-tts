#!/usr/bin/env python3
"""Score an existing acoustic manifest with a caller-verified UTMOS checkout."""

import argparse
import json
import pathlib
import sys

MAX_CASES = 128
MAX_WAVE_BYTES = 64 * 1024 * 1024


def regular_file(path: pathlib.Path, maximum_bytes: int | None = None) -> pathlib.Path:
    path = path.resolve(strict=True)
    stat = path.stat()
    if not path.is_file() or (maximum_bytes is not None and stat.st_size > maximum_bytes):
        raise ValueError("input is not a bounded regular file")
    return path


def wave_path(root: pathlib.Path, relative: str) -> pathlib.Path:
    part = pathlib.PurePath(relative)
    if part.is_absolute() or ".." in part.parts or not part.parts:
        raise ValueError("WAV path is not a safe relative path")
    path = root.joinpath(part)
    if path.is_symlink():
        raise ValueError("WAV symlinks are not accepted")
    return regular_file(path, MAX_WAVE_BYTES)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=pathlib.Path, required=True)
    parser.add_argument("--speechmos-source", type=pathlib.Path, required=True)
    parser.add_argument("--model", type=pathlib.Path, required=True)
    args = parser.parse_args()

    manifest_path = regular_file(args.manifest, 1024 * 1024)
    model_path = regular_file(args.model)
    source = args.speechmos_source.resolve(strict=True)
    if not source.is_dir():
        raise ValueError("SpeechMOS source is not a directory")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    cases = manifest.get("cases")
    if manifest.get("schema") != "utterpipe.acoustic-manifest/1" or not isinstance(cases, list):
        raise ValueError("unsupported acoustic manifest")
    if not 1 <= len(cases) <= MAX_CASES:
        raise ValueError("acoustic case count is out of bounds")

    sys.path.insert(0, str(source))
    import librosa  # pylint: disable=import-outside-toplevel
    import torch  # pylint: disable=import-outside-toplevel
    from speechmos.utmos22.strong.model import UTMOS22Strong  # pylint: disable=import-outside-toplevel

    model = UTMOS22Strong()
    model.load_state_dict(torch.load(model_path, map_location="cpu", weights_only=True))
    model.eval()
    root = manifest_path.parent
    seen = set()
    scores = []
    with torch.inference_mode():
        for case in cases:
            case_id = case.get("id")
            candidate = case.get("candidate_wavs")
            baseline = case.get("baseline_wavs")
            if (
                not isinstance(case_id, str)
                or case_id in seen
                or not isinstance(candidate, list)
                or not candidate
                or not isinstance(baseline, list)
                or not baseline
            ):
                raise ValueError("acoustic case layout is invalid")
            seen.add(case_id)
            values = []
            for relative in (candidate[0], baseline[0]):
                path = wave_path(root, relative)
                wave, sample_rate = librosa.load(path, sr=None, mono=True)
                tensor = torch.from_numpy(wave).unsqueeze(0)
                values.append(float(model(tensor, sample_rate)[0]))
            if not all(value == value and abs(value) != float("inf") for value in values):
                raise ValueError("perceptual model returned a non-finite score")
            scores.append({"id": case_id, "candidate": values[0], "baseline": values[1]})

    json.dump({"schema": "utterpipe.pocket-tts.xn-utmos22-scores/1", "cases": scores}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
