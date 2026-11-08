"""Draws a book's illustrations: Stable Diffusion XL on the GPU.

Run by tts_server.py in the voice designer's environment (data/design-venv), which stops the
performer first (they don't both fit on the card):

    illustrate.py '{"jobs": [{"prompt": "...", "negative": "...", "seed": 7, "out": ".../x.png"}],
                    "width": 1216, "height": 832, "steps": 30}'

Prints one JSON line per picture made ({"out", "seconds"}).
"""

import json
import os
import sys
import time

import torch

MODEL = os.environ.get("DUET_IMAGE_MODEL", "stabilityai/stable-diffusion-xl-base-1.0")
# (SDXL's own VAE overflows in half precision and draws black; this one is the same, fixed.)
VAE = "madebyollin/sdxl-vae-fp16-fix"
NEGATIVE = (
    "text, words, letters, caption, watermark, signature, logo, frame, border, blurry, lowres, "
    "jpeg artifacts, deformed, disfigured, extra limbs, extra fingers, bad anatomy, bad hands, "
    "duplicate, cropped, out of frame, ugly, photo, photograph"
)


def main():
    job = json.loads(sys.argv[1])
    from diffusers import AutoencoderKL, StableDiffusionXLPipeline

    vae = AutoencoderKL.from_pretrained(VAE, torch_dtype=torch.float16)
    pipe = StableDiffusionXLPipeline.from_pretrained(MODEL, vae=vae, torch_dtype=torch.float16, variant="fp16", use_safetensors=True)
    pipe.to("cuda")
    pipe.set_progress_bar_config(disable=True)
    w, h, steps = int(job.get("width", 1216)), int(job.get("height", 832)), int(job.get("steps", 30))
    for j in job["jobs"]:
        t = time.time()
        gen = torch.Generator("cuda").manual_seed(int(j.get("seed", 0)))
        img = pipe(
            prompt=j["prompt"],
            prompt_2=j.get("prompt_2") or j["prompt"],
            negative_prompt=(j.get("negative") or "") + ", " + NEGATIVE,
            width=w,
            height=h,
            num_inference_steps=steps,
            guidance_scale=float(j.get("guidance", 6.5)),
            generator=gen,
        ).images[0]
        os.makedirs(os.path.dirname(j["out"]), exist_ok=True)
        tmp = j["out"] + ".part.png"
        img.save(tmp)
        os.replace(tmp, j["out"])
        print(json.dumps({"out": j["out"], "seconds": round(time.time() - t, 1)}), flush=True)


if __name__ == "__main__":
    main()
