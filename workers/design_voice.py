"""Makes a voice's reference clip from its description, in any accent.

Run by tts_server.py in its own environment (data/design-venv: these models need a newer
transformers than Qwen3-TTS):

    design_voice.py '{"description": "...", "text": "...", "out": "voices/x.wav"}'

and prints {"engine", "accent", "score"} as JSON on its last line.

Two designers, because neither does everything:
  * OmniVoice (k2-fsa) takes plain attributes (sex, age down to "child", pitch) and ten English
    accents: American, British, Australian, Canadian, Indian, Chinese, Korean, Japanese,
    Portuguese and Russian. Quick, and reliably in the accent asked for.
  * Maya1 (Maya Research) takes the description as written, so any accent a book needs
    (Scottish, Irish, French, German...). Slower: used for accents OmniVoice doesn't have.
Fish, which performs the lines, keeps the accent of the clip it's given, so the clip is what
decides how a character sounds. Where an accent classifier knows the accent, a few takes are
made and the one that sounds most like it is kept.
"""

import json
import re
import sys

import numpy as np
import soundfile as sf
import torch

RATE = 24000

# OmniVoice's accents, by the words a description uses for them.
OMNI_ACCENTS = [
    ("british", r"british|english|englishm[ae]n|englishwom[ae]n|england|received pronunciation|\brp\b|london|cockney|oxford|yorkshire|bbc"),
    ("american", r"american|united states|new york|texan|southern drawl|\bus\b"),
    ("australian", r"australian|australia|aussie"),
    ("canadian", r"canadian|canada"),
    ("indian", r"\bindian\b|\bindia\b"),
    ("chinese", r"chinese|china|mandarin|cantonese"),
    ("korean", r"korean|korea"),
    ("japanese", r"japanese|japan"),
    ("portuguese", r"portuguese|portugal|brazilian"),
    ("russian", r"russian|russia|soviet"),
]
# Accents only Maya1 can do (anything else said before "accent" goes to it too).
OTHER_ACCENTS = (
    r"scottish|scots|glasgow|edinburgh|irish|ireland|dublin|welsh|wales|french|france|german|germany|"
    r"italian|italy|spanish|spain|mexican|dutch|swedish|norwegian|danish|finnish|polish|czech|"
    r"hungarian|greek|turkish|arabic|egyptian|persian|israeli|african|nigerian|kenyan|jamaican|"
    r"caribbean|new zealand|kiwi|south african|filipino|vietnamese|thai|ukrainian|yiddish|"
    r"cornish|geordie|scouse|liverpool|brooklyn|boston"
)
# The accent classifier's labels (CommonAccent), where it knows the accent.
LABELS = {
    "british": "england", "american": "us", "australian": "australia", "canadian": "canada",
    "indian": "indian", "scottish": "scotland", "irish": "ireland", "welsh": "wales",
    "new zealand": "newzealand", "south african": "african",
}


def accent_of(text):
    """(accent, OmniVoice can do it): the first accent the description names."""
    found = [(m.start(), name, True) for name, pat in OMNI_ACCENTS for m in [re.search(pat, text)] if m]
    other = re.search(OTHER_ACCENTS, text)
    if other:
        word = other.group(0)
        name = {"scots": "scottish", "glasgow": "scottish", "edinburgh": "scottish", "ireland": "irish", "dublin": "irish",
                "wales": "welsh", "kiwi": "new zealand"}.get(word, word)
        found.append((other.start(), name, False))
    if not found:
        return None, True
    _, name, omni = min(found)
    return name, omni


def sex_of(text):
    f = re.search(r"\b(female|woman|girl|lady|she|her|queen|duchess|mother|sister|englishwoman)\b", text)
    m = re.search(r"\b(male|man|boy|gentleman|he|his|king|duke|father|brother|englishman)\b", text)
    if f and (not m or f.start() < m.start()):
        return "female"
    return "male" if m else None


def age_of(text):
    n = r"([4-9]|1[0-2]|four|five|six|seven|eight|nine|ten|eleven|twelve)"
    if re.search(r"\b(child|kid|toddler|little girl|little boy|young girl|young boy|pre-pubescent|schoolgirl|schoolboy)\b", text) or re.search(
        rf"\b(about|aged|age|of)\s+{n}\b|\b{n}[- ]years?[- ]old\b", text
    ):
        return "child"
    if re.search(r"\b(teen|teenage|adolescent|1[3-9]\b)", text):
        return "teenager"
    if re.search(r"\b(middle-aged|middle aged|40s|50s|forties|fifties|about 4\d|about 5\d)\b", text):
        return "middle-aged"
    if re.search(r"\b(elderly|old man|old woman|ancient|60s|70s|80s|sixties|seventies|eighties)\b", text):
        return "elderly"
    if re.search(r"\b(young|20s|30s|twenties|thirties|about [23]\d)\b", text):
        return "young adult"
    return None


def pitch_of(text):
    if re.search(r"\b(very deep|booming|bass)\b", text):
        return "very low pitch"
    if re.search(r"\b(deep|low|baritone|gravelly|husky)\b", text):
        return "low pitch"
    if re.search(r"\b(squeaky|shrill|piping)\b", text):
        return "very high pitch"
    if re.search(r"\b(high|bright|light|soprano)\b", text):
        return "high pitch"
    return None


class Classifier:
    def __init__(self, savedir):
        from speechbrain.inference.classifiers import EncoderClassifier

        self.m = EncoderClassifier.from_hparams(source="Jzuluaga/accent-id-commonaccent_ecapa", savedir=savedir, run_opts={"device": "cpu"})

    def score(self, wav, label):
        import torchaudio

        x = torchaudio.functional.resample(torch.as_tensor(wav, dtype=torch.float32).reshape(1, -1), RATE, 16000)
        out, _, _, _ = self.m.classify_batch(x)
        p = torch.softmax(out.squeeze(), dim=0)
        enc = self.m.hparams.label_encoder
        return float(p[enc.encode_label(label)]), enc.decode_ndim(int(p.argmax()))


def omni(text, instruct):
    from omnivoice import OmniVoice

    m = OmniVoice.from_pretrained("k2-fsa/OmniVoice", device_map="cuda:0", dtype=torch.float16)
    return lambda: np.asarray(m.generate(text=text, instruct=instruct)[0], dtype=np.float32)


def maya(text, description):
    from snac import SNAC
    from transformers import AutoModelForCausalLM, AutoTokenizer

    model = AutoModelForCausalLM.from_pretrained("maya-research/maya1", torch_dtype=torch.bfloat16, device_map="cuda:0")
    tok = AutoTokenizer.from_pretrained("maya-research/maya1")
    snac = SNAC.from_pretrained("hubertsiuzdak/snac_24khz").eval().to("cuda")
    t = lambda i: tok.decode([i])
    prompt = t(128259) + tok.bos_token + f'<description="{description}"> {text}' + t(128009) + t(128260) + t(128261) + t(128257)
    ins = tok(prompt, return_tensors="pt", add_special_tokens=False).to("cuda")

    def take():
        with torch.inference_mode():
            out = model.generate(
                **ins, max_new_tokens=2048, min_new_tokens=28, temperature=0.4, top_p=0.9,
                repetition_penalty=1.1, do_sample=True, eos_token_id=128258, pad_token_id=tok.pad_token_id,
            )
        ids = out[0, ins["input_ids"].shape[1]:].tolist()
        if 128258 in ids:
            ids = ids[: ids.index(128258)]
        codes = [i for i in ids if 128266 <= i <= 156937]
        codes = [(c - 128266) % 4096 for c in codes[: len(codes) // 7 * 7]]
        l1, l2, l3 = [], [], []
        for f in range(len(codes) // 7):
            s = codes[f * 7 : f * 7 + 7]
            l1.append(s[0])
            l2 += [s[1], s[4]]
            l3 += [s[2], s[3], s[5], s[6]]
        levels = [torch.tensor(l, device="cuda").unsqueeze(0) for l in (l1, l2, l3)]
        with torch.inference_mode():
            wav = snac.decoder(snac.quantizer.from_codes(levels))[0, 0].float().cpu().numpy()
        return wav[2048:]

    return take


def main():
    job = json.loads(sys.argv[1])
    desc = (job.get("description") or "A warm, clear, expressive audiobook narrator.").strip()
    text = job["text"]
    low = desc.lower()
    accent, omni_can = accent_of(low)
    if omni_can:
        attrs = [a for a in (sex_of(low), age_of(low), pitch_of(low)) if a]
        if accent:
            attrs.append(f"{accent} accent")
        engine, make, tries = "omnivoice", omni(text, ", ".join(attrs)), 4
        print(f"  OmniVoice: {', '.join(attrs)}", file=sys.stderr, flush=True)
    else:
        # (Said plainly, first: a long description confuses it.)
        engine, make, tries = "maya1", maya(text, desc), 2
        print(f"  Maya1 ({accent} accent)", file=sys.stderr, flush=True)
    label = LABELS.get(accent)
    judge = None
    if label:
        try:
            import os

            judge = Classifier(os.path.join(os.path.dirname(os.path.abspath(job["out"])), ".accent-model"))
        except Exception as e:
            print(f"  accent check unavailable: {e}", file=sys.stderr, flush=True)
    best = None
    for i in range(tries if judge else 1):
        wav = make()
        if judge is None:
            best = (None, wav)
            break
        score, top = judge.score(wav, label)
        print(f"  take {i + 1}: {label} {score:.2f} (sounds most like {top})", file=sys.stderr, flush=True)
        if best is None or score > best[0]:
            best = (score, wav)
        if top == label:
            break
    sf.write(job["out"], best[1], RATE)
    print(json.dumps({"engine": engine, "accent": accent, "score": best[0]}))


if __name__ == "__main__":
    main()
