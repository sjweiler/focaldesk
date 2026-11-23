#!/usr/bin/env python3
"""Generate a FocalDesk AI HDR treatment grid from an SDR wallpaper."""

import argparse
import base64
import http.client
import io
import json
import os

from PIL import Image


PROMPT = """Analyze this SDR wallpaper for HDR highlight enhancement.
Divide it into a uniform 4×4 grid and score the strongest qualifying highlight
in each cell:
0 = no qualifying highlight, including diffuse bright surfaces and shadows
1 = subtle specular reflection or weak illumination
2 = moderate reflection or localized illumination
3 = strong light source, atmospheric glow, or intense specular reflection
4 = strongest visible light source or highlight in the entire image

Ignore text, labels, and interface elements. Brightness alone does not make
a diffuse surface a light source. Return all zeros if no highlights qualify.
Return only a JSON object with a "strength" array of 16 integers in row-major
order (left to right, top to bottom)."""

FORMAT = {
    "type": "object",
    "properties": {
        "strength": {
            "type": "array",
            "items": {"type": "integer", "minimum": 0, "maximum": 4},
            "minItems": 16,
            "maxItems": 16,
        }
    },
    "required": ["strength"],
    "additionalProperties": False,
}


def generate_mask(path, model, timeout):
    with Image.open(path) as wallpaper:
        preview = wallpaper.convert("RGB")
        preview.thumbnail((768, 768))
        with io.BytesIO() as buffer:
            preview.save(buffer, format="JPEG")
            encoded = base64.b64encode(buffer.getvalue()).decode("ascii")

    payload = {
        "model": model,
        "stream": False,
        "think": False,
        "keep_alive": 0,
        "format": FORMAT,
        "options": {"temperature": 0, "num_predict": 128},
        "prompt": PROMPT,
        "images": [encoded],
    }
    connection = http.client.HTTPConnection("127.0.0.1", 11434, timeout=timeout)
    try:
        connection.request(
            "POST", "/api/generate", json.dumps(payload),
            {"Content-Type": "application/json"},
        )
        response = connection.getresponse()
        body = response.read()
        if response.status != 200:
            raise ValueError(f"Ollama HTTP {response.status}: {body.decode(errors='replace')}")
    finally:
        connection.close()

    result = json.loads(body)
    if not isinstance(result, dict) or not isinstance(result.get("response"), str):
        raise ValueError("Ollama did not return a model response")
    mask = json.loads(result["response"])
    if not isinstance(mask, dict) or mask.keys() != {"strength"}:
        raise ValueError('Expected a JSON object with only a "strength" field')
    scores = mask["strength"]
    if not isinstance(scores, list) or len(scores) != 16 or any(
        type(score) is not int or not 0 <= score <= 4 for score in scores
    ):
        raise ValueError("Expected 16 integer scores between 0 and 4")
    return mask


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("wallpaper", help="SDR image to analyze (requires Pillow)")
    parser.add_argument("--model", default=os.getenv("FOCALDESK_WALLPAPER_HDR_MODEL", "gemma4:12b"))
    parser.add_argument("--timeout", type=int, help="Ollama socket timeout in seconds")
    args = parser.parse_args()

    timeout = args.timeout
    if timeout is None:
        try:
            timeout = int(os.getenv("FOCALDESK_WALLPAPER_HDR_TIMEOUT_SECS", "45"))
        except ValueError:
            timeout = 45
        if not 1 <= timeout <= 90:
            timeout = 45
    elif timeout <= 0:
        parser.error("--timeout must be positive")

    try:
        mask = generate_mask(args.wallpaper, args.model, timeout)
    except (OSError, ValueError, http.client.HTTPException) as error:
        parser.exit(1, f"{parser.prog}: {error}\n")
    print(json.dumps(mask))


if __name__ == "__main__":
    main()
