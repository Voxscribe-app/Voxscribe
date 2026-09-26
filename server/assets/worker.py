"""Long-lived NeMo worker.

Speaks newline-delimited JSON on stdout, each protocol line prefixed with
PROTOCOL_PREFIX so ordinary NeMo/PyTorch chatter can share the stream.
"""

import json
import os
import sys
import time
import traceback

PROTOCOL_PREFIX = "@@VOXSCRIBE@@"

MODEL_NAME = os.environ.get("VOXSCRIBE_NEMO_MODEL", "nvidia/parakeet-unified-en-0.6b")
REQUESTED_PRECISION = os.environ.get("VOXSCRIBE_NEMO_PRECISION", "auto").lower()


def send(payload):
    sys.stdout.write(PROTOCOL_PREFIX + json.dumps(payload, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def fail(message, request_id=None):
    payload = {"type": "error", "message": str(message)}
    if request_id is not None:
        payload["id"] = request_id
    send(payload)


def gib(value):
    return value / (1024**3)


def resolve_precision(capability):
    if REQUESTED_PRECISION != "auto":
        if REQUESTED_PRECISION not in ("fp32", "fp16"):
            raise RuntimeError(
                f"unsupported precision: {REQUESTED_PRECISION}; use auto, fp32, or fp16"
            )
        return REQUESTED_PRECISION
    # Pascal and older have no fast half-precision path.
    return "fp32" if capability[0] < 7 else "fp16"


def load_model():
    import torch
    import nemo.collections.asr as nemo_asr
    from omegaconf import OmegaConf, open_dict

    if not torch.cuda.is_available():
        raise RuntimeError(
            "torch.cuda.is_available() is false; the worker needs a CUDA PyTorch build"
        )

    gpu_name = torch.cuda.get_device_name(0)
    capability = torch.cuda.get_device_capability(0)
    precision = resolve_precision(capability)

    torch.set_grad_enabled(False)
    started = time.perf_counter()

    model = nemo_asr.models.ASRModel.from_pretrained(model_name=MODEL_NAME)

    # Some checkpoints ship without validation_ds, which .cuda() then trips over.
    with open_dict(model.cfg):
        if model.cfg.get("validation_ds") is None:
            model.cfg.validation_ds = OmegaConf.create({})

    model = model.cuda()
    if precision == "fp16":
        model = model.half()
    model.eval()

    torch.cuda.synchronize()
    torch.cuda.empty_cache()

    ready = {
        "type": "ready",
        "model": MODEL_NAME,
        "gpu": gpu_name,
        "compute_capability": f"{capability[0]}.{capability[1]}",
        "precision": precision,
        "load_seconds": time.perf_counter() - started,
        "torch_version": torch.__version__,
        "torch_cuda": torch.version.cuda,
    }
    return model, torch, ready


def transcribe(model, torch, request):
    import soundfile as sf

    path = request.get("path")
    if not path:
        raise RuntimeError("missing path")

    audio_seconds = float(sf.info(path).duration)

    torch.cuda.empty_cache()
    torch.cuda.reset_peak_memory_stats()
    torch.cuda.synchronize()

    started = time.perf_counter()
    result = model.transcribe([path], batch_size=1, verbose=False)
    torch.cuda.synchronize()
    inference_seconds = time.perf_counter() - started

    peak_allocated_gib = gib(torch.cuda.max_memory_allocated())
    peak_reserved_gib = gib(torch.cuda.max_memory_reserved())
    torch.cuda.empty_cache()

    transcript = result[0]
    text = transcript.text if hasattr(transcript, "text") else str(transcript)

    return {
        "type": "result",
        "id": request.get("id"),
        "text": text,
        "audio_seconds": audio_seconds,
        "inference_seconds": inference_seconds,
        "realtime_x": (audio_seconds / inference_seconds) if inference_seconds > 0 else None,
        "peak_allocated_gib": peak_allocated_gib,
        "peak_reserved_gib": peak_reserved_gib,
    }


def main():
    model, torch, ready = load_model()
    send(ready)

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        request_id = None
        try:
            request = json.loads(line)
            request_id = request.get("id")
            command = request.get("cmd")

            if command == "shutdown":
                send({"type": "shutdown", "id": request_id})
                return
            if command != "transcribe":
                fail(f"unknown command: {command}", request_id)
                continue

            send(transcribe(model, torch, request))
        except Exception as exc:
            traceback.print_exc(file=sys.stderr)
            fail(exc, request_id)


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        traceback.print_exc(file=sys.stderr)
        fail(exc)
        raise
