#!/usr/bin/env python3
"""Add sherpa-onnx metadata to the pinned Kokoro v0.19 Q8 model."""

import argparse
from pathlib import Path

import onnx


SPEAKERS = [
    "af",
    "af_bella",
    "af_nicole",
    "af_sarah",
    "af_sky",
    "am_adam",
    "am_michael",
    "bf_emma",
    "bf_isabella",
    "bm_george",
    "bm_lewis",
]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("model", type=Path)
    args = parser.parse_args()

    model = onnx.load_model(args.model)
    speaker_to_id = ",".join(
        f"{speaker}->{index}" for index, speaker in enumerate(SPEAKERS)
    )
    id_to_speaker = ",".join(
        f"{index}->{speaker}" for index, speaker in enumerate(SPEAKERS)
    )
    onnx.helper.set_model_props(
        model,
        {
            "model_type": "kokoro",
            "language": "English",
            "has_espeak": "1",
            "sample_rate": "24000",
            "version": "1",
            "voice": "en-us",
            "style_dim": "511,1,256",
            "n_speakers": str(len(SPEAKERS)),
            "speaker2id": speaker_to_id,
            "id2speaker": id_to_speaker,
            "speaker_names": ",".join(SPEAKERS),
            "model_url": "https://github.com/thewh1teagle/kokoro-onnx/releases/tag/model-files",
            "maintainer": "FastTalk",
            "comment": "Kokoro v0.19 English Q8 with sherpa-onnx metadata",
        },
    )
    onnx.save_model(model, args.model)


if __name__ == "__main__":
    main()
