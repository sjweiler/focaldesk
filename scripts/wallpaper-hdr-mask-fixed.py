#!/usr/bin/env python3
"""Generate a FocalDesk AI HDR treatment grid from an SDR wallpaper."""

import argparse
import base64
import http.client
import io
import json
import os

from PIL import Image


PROMPT = """Analyze this wallpaper to identify regions suitable for
HDR highlight enhancement.

Identify visible light sources, illuminated atmospheric
edges, emissive objects, and specular reflections.

Divide the image into a uniform 4×4 grid.

Assign one integer score to each cell:

0 = No qualifying HDR highlight. Ordinary surfaces,
    diffuse bright objects, and shadows.

1 = Subtle specular reflection or weak illumination.

2 = Moderate reflection or localized illumination.

3 = Strong visible light source, atmospheric glow,
    or intense specular reflection.

4 = Strongest visible light source or highlight
    in the entire image.

Rules:
- Score the strongest qualifying highlight in each cell.
- Do not classify ordinary white or bright surfaces
  as light sources solely because of their brightness.
- Preserve the distinction between emissive light
  and diffuse reflected light.
- Do not boost shadows or ordinary surfaces.
- Ignore text, labels, and interface elements.
- If no qualifying highlights exist, return all zeros.
- Return exactly 16 integers in row-major order.
- Every integer must be between 0 and 4.
- Return only valid JSON, with no Markdown or explanation.

Required output:
{"strength":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}"""

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
