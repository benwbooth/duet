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

# (Memory handed out in growable pieces: less of it lost to fragments.)
os.environ.setdefault("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")

import torch

MODEL = os.environ.get("DUET_IMAGE_MODEL", "stabilityai/stable-diffusion-xl-base-1.0")
# (SDXL's own VAE overflows in half precision and draws black; this one is the same, fixed.)
VAE = "madebyollin/sdxl-vae-fp16-fix"
NEGATIVE = (
    "text, words, letters, caption, watermark, signature, logo, frame, border, blurry, lowres, "
    "jpeg artifacts, deformed, disfigured, extra limbs, extra fingers, bad anatomy, bad hands, "
    "duplicate, cropped, out of frame, ugly, photo, photograph"
)


def int8(model):
    """Big linear layers held as 8-bit integers with a scale per output channel (as for the
    performer, see fish_launch.py): half the memory, so the image model and its text model both
    fit on the card beside other work instead of spilling into system memory and crawling."""
    from torch import nn

    class Int8Linear(nn.Module):
        def __init__(self, linear):
            super().__init__()
            w = linear.weight.data.float()
            scale = w.abs().amax(dim=1).clamp(min=1e-8) / 127.0
            self.register_buffer("weight", torch.round(w / scale[:, None]).clamp(-127, 127).to(torch.int8))
            self.register_buffer("scale", scale.to(torch.bfloat16))
            self.bias = linear.bias
            self.in_features, self.out_features = linear.in_features, linear.out_features

        def forward(self, x):
            out = nn.functional.linear(x, self.weight.to(x.dtype)) * self.scale.to(x.dtype)
            return out + self.bias if self.bias is not None else out

    for module in list(model.modules()):
        for name, child in list(module.named_children()):
            if isinstance(child, nn.Linear) and child.weight.numel() >= 1 << 20:
                setattr(module, name, Int8Linear(child))
    return model


def zimage_pipe():
    """Z-Image-Turbo (Alibaba Tongyi, Apache-2.0): far better at drawing the scene as described
    than SDXL, with a full language model reading the prompt. Both its models in 8-bit, so they
    fit on the card together."""
    from diffusers import ZImagePipeline

    t0 = time.time()
    pipe = ZImagePipeline.from_pretrained("Tongyi-MAI/Z-Image-Turbo", torch_dtype=torch.bfloat16)
    pipe.set_progress_bar_config(disable=True)
    int8(pipe.transformer)
    int8(pipe.text_encoder)
    pipe.to("cuda")
    # (The decoder a quarter of the picture at a time: whole, its last layers wanted more than
    # the card had left beside other work.)
    pipe.vae.enable_tiling()
    pipe.vae.tile_sample_min_size = 384
    pipe.vae.tile_latent_min_size = 48
    pipe.vae.tile_overlap_factor = 0.25
    print(json.dumps({"ready": round(time.time() - t0, 1)}), file=sys.stderr, flush=True)
    return pipe


def zimage(job, pipe=None):
    pipe = pipe or zimage_pipe()
    for j in job["jobs"]:
        t = time.time()
        with torch.inference_mode():
            img = pipe(
                prompt=j.get("full_prompt") or j["prompt"],
                # (They're shown at half a column's width: this is plenty, and lighter to draw.)
                width=int(j.get("width", job.get("width", 1024))),
                height=int(j.get("height", job.get("height", 704))),
                num_inference_steps=9,
                guidance_scale=0.0,
                generator=torch.Generator("cuda").manual_seed(int(j.get("seed", 0))),
            ).images[0]
        os.makedirs(os.path.dirname(j["out"]), exist_ok=True)
        tmp = j["out"] + ".part.png"
        img.save(tmp)
        os.replace(tmp, j["out"])
        print(json.dumps({"out": j["out"], "seconds": round(time.time() - t, 1)}), flush=True)


def serve():
    """Kept running (loading the model takes a minute or more): a job per line on stdin, the
    pictures made reported on stdout, then {"done": true}."""
    pipe = zimage_pipe()
    print(json.dumps({"ready": True}), flush=True)
    for line in sys.stdin:
        if not line.strip():
            continue
        try:
            zimage(json.loads(line), pipe)
        except Exception as e:
            print(json.dumps({"error": str(e)}), flush=True)
        print(json.dumps({"done": True}), flush=True)


def main():
    if sys.argv[1] == "--serve":
        return serve()
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
