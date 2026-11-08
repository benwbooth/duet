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


def zimage(job):
    """Z-Image-Turbo (Alibaba Tongyi, Apache-2.0): far better at drawing the scene as described
    than SDXL, with a full language model reading the prompt. The text model and the image
    model don't both fit on the card beside other work, so: every prompt read first, with the
    text model alone on the card, then the image model alone draws them all (moving the two on
    and off for every picture took twice as long)."""
    from diffusers import ZImagePipeline

    t0 = time.time()
    pipe = ZImagePipeline.from_pretrained("Tongyi-MAI/Z-Image-Turbo", torch_dtype=torch.bfloat16)
    pipe.set_progress_bar_config(disable=True)
    pipe.text_encoder.to("cuda")
    with torch.inference_mode():
        embeds = [pipe.encode_prompt(j.get("full_prompt") or j["prompt"], device="cuda", do_classifier_free_guidance=False)[0] for j in job["jobs"]]
    pipe.text_encoder.to("cpu")
    torch.cuda.empty_cache()
    pipe.transformer.to("cuda")
    pipe.vae.to("cuda")
    print(json.dumps({"ready": round(time.time() - t0, 1)}), file=sys.stderr, flush=True)
    for j, emb in zip(job["jobs"], embeds):
        t = time.time()
        img = pipe(
            prompt_embeds=emb,
            width=int(j.get("width", job.get("width", 1216))),
            height=int(j.get("height", job.get("height", 832))),
            num_inference_steps=9,
            guidance_scale=0.0,
            generator=torch.Generator("cuda").manual_seed(int(j.get("seed", 0))),
        ).images[0]
        os.makedirs(os.path.dirname(j["out"]), exist_ok=True)
        tmp = j["out"] + ".part.png"
        img.save(tmp)
        os.replace(tmp, j["out"])
        print(json.dumps({"out": j["out"], "seconds": round(time.time() - t, 1)}), flush=True)


def main():
    job = json.loads(sys.argv[1])
    if job.get("model") == "z-image":
        return zimage(job)
    from diffusers import AutoencoderKL, StableDiffusionXLPipeline
    from PIL import Image

    vae = AutoencoderKL.from_pretrained(VAE, torch_dtype=torch.float16)
    pipe = StableDiffusionXLPipeline.from_pretrained(MODEL, vae=vae, torch_dtype=torch.float16, variant="fp16", use_safetensors=True)
    pipe.to("cuda")
    pipe.set_progress_bar_config(disable=True)
    w, h, steps = int(job.get("width", 1216)), int(job.get("height", 832)), int(job.get("steps", 30))
    # Pictures drawn from a character's portrait (IP-Adapter): the face and look carried over from
    # it, so a character looks like themselves in every scene. The rest first (the portraits
    # themselves among them), then the adapter is loaded for these.
    plain = [j for j in job["jobs"] if not j.get("ref")]
    referred = [j for j in job["jobs"] if j.get("ref")]
    for group in (plain, referred):
        if not group:
            continue
        if group is referred:
            pipe.load_ip_adapter(
                "h94/IP-Adapter", subfolder="sdxl_models", weight_name="ip-adapter-plus-face_sdxl_vit-h.safetensors",
                image_encoder_folder="models/image_encoder",
            )
        for j in group:
            t = time.time()
            gen = torch.Generator("cuda").manual_seed(int(j.get("seed", 0)))
            extra = {}
            ref = j.get("ref")
            if ref and os.path.exists(ref):
                pipe.set_ip_adapter_scale(float(j.get("ref_scale", 0.45)))
                extra["ip_adapter_image"] = Image.open(ref).convert("RGB")
            elif group is referred:
                # (Its portrait isn't there: drawn from the words alone.)
                pipe.set_ip_adapter_scale(0.0)
                extra["ip_adapter_image"] = Image.new("RGB", (224, 224), "white")
            img = pipe(
                prompt=j["prompt"],
                prompt_2=j.get("prompt_2") or j["prompt"],
                negative_prompt=(j.get("negative") or "") + ", " + NEGATIVE,
                width=int(j.get("width", w)),
                height=int(j.get("height", h)),
                num_inference_steps=steps,
                guidance_scale=float(j.get("guidance", 6.5)),
                generator=gen,
                **extra,
            ).images[0]
            os.makedirs(os.path.dirname(j["out"]), exist_ok=True)
            tmp = j["out"] + ".part.png"
            img.save(tmp)
            os.replace(tmp, j["out"])
            print(json.dumps({"out": j["out"], "seconds": round(time.time() - t, 1)}), flush=True)


if __name__ == "__main__":
    main()
