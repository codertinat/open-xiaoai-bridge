"""Sherpa-ONNX offline ASR using SenseVoice model.

Provides local speech-to-text recognition for the OpenClaw conversation flow.
The model is lazily loaded on first use to avoid blocking startup.
"""

import os

import numpy as np
import sherpa_onnx

from core.utils.file import get_model_file_path
from core.utils.logger import logger


class _SherpaASR:
    """Wrapper around sherpa_onnx.OfflineRecognizer with SenseVoice."""

    def __init__(self):
        self._recognizer = None

    def _find_model_dir(self) -> str:
        """Scan core/models/ for a directory containing model.int8.onnx."""
        models_root = get_model_file_path("")
        for entry in os.scandir(models_root):
            if entry.is_dir():
                if os.path.isfile(os.path.join(entry.path, "model.int8.onnx")):
                    return entry.path
        raise FileNotFoundError(
            f"No SenseVoice model found in {models_root}. "
            "Please place the sherpa-onnx-sense-voice-* directory under core/models/."
        )

    def _ensure_loaded(self):
        """Lazily initialize the OfflineRecognizer on first use."""
        if self._recognizer is not None:
            return

        model_dir = self._find_model_dir()
        model_path = os.path.join(model_dir, "model.int8.onnx")
        tokens_path = os.path.join(model_dir, "tokens.txt")

        logger.info(f"[ASR] Loading SenseVoice model from {model_dir}", module="ASR")

        self._recognizer = sherpa_onnx.OfflineRecognizer.from_sense_voice(
            model=model_path,
            tokens=tokens_path,
            num_threads=2,
            use_itn=True,
            debug=False,
            provider="cpu",
            language="auto",
        )
        logger.info("[ASR] SenseVoice model loaded", module="ASR")

    def asr(self, pcm_bytes: bytes, sample_rate: int = 16000) -> str:
        """Recognize speech from raw PCM int16 audio bytes.

        Args:
            pcm_bytes: Raw PCM audio data (int16, mono).
            sample_rate: Sample rate of the audio (default 16000).

        Returns:
            Recognized text string, or empty string if nothing recognized.
        """
        self._ensure_loaded()

        samples = np.frombuffer(pcm_bytes, dtype=np.int16)
        samples = samples.astype(np.float32) / 32768.0

        stream = self._recognizer.create_stream()
        stream.accept_waveform(sample_rate, samples)
        self._recognizer.decode_stream(stream)

        text = stream.result.text.strip()
        if text:
            logger.info(f"[ASR] Recognized: {text}", module="ASR")
        else:
            logger.debug("[ASR] No speech recognized", module="ASR")
        return text


SherpaASR = _SherpaASR()
