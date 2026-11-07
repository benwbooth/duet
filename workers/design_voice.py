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


class Ears:
    """How much of the line a take actually says (0..1): Whisper's words against the text. A
    designer sometimes makes a take that's only noise or babble, which Fish then copies."""

    def __init__(self):
        from transformers import pipeline

        self.asr = pipeline("automatic-speech-recognition", model="openai/whisper-large-v3-turbo", device="cuda:0", torch_dtype=torch.float16)

    def clarity(self, wav, text):
        import difflib

        import torchaudio

        x = torchaudio.functional.resample(torch.as_tensor(wav, dtype=torch.float32), RATE, 16000).numpy()
        heard = self.asr({"raw": x, "sampling_rate": 16000}, generate_kwargs={"language": "en"})["text"]
        words = lambda t: re.findall(r"[a-z']+", t.lower())
        want, got = words(text), words(heard)
        m = difflib.SequenceMatcher(None, want, got)
        return m.ratio() if want else 1.0


def main():
    import os

    job = json.loads(sys.argv[1])
    desc = (job.get("description") or "A warm, clear, expressive audiobook narrator.").strip()
    text = job["text"]
    low = desc.lower()
    accent, omni_can = accent_of(low)
    label = LABELS.get(accent)
    judge = None
    if label:
        try:
            judge = Classifier(os.path.join(os.path.dirname(os.path.abspath(job["out"])), ".accent-model"))
        except Exception as e:
            print(f"  accent check unavailable: {e}", file=sys.stderr, flush=True)
    try:
        ears = Ears()
    except Exception as e:
        ears = None
        print(f"  clarity check unavailable: {e}", file=sys.stderr, flush=True)

    # Rounds of takes; a child voice that won't come out clear is tried again a little older.
    rounds = []
    if omni_can:
        sex, age, pitch = sex_of(low), age_of(low), pitch_of(low)
        ages = [age, "teenager"] if age == "child" else [age]
        for a in ages:
            attrs = [x for x in (sex, a, pitch) if x] + ([f"{accent} accent"] if accent else [])
            rounds.append(("omnivoice", ", ".join(attrs), 4))
    else:
        rounds.append(("maya1", desc, 3))
    best = None  # (clear, accent score, clarity, wav, engine)
    for engine, prompt, tries in rounds:
        print(f"  {engine}: {prompt[:120]}", file=sys.stderr, flush=True)
        make = omni(text, prompt) if engine == "omnivoice" else maya(text, prompt)
        for i in range(tries):
            wav = make()
            clarity = ears.clarity(wav, text) if ears else 1.0
            score, top = judge.score(wav, label) if judge else (0.0, None)
            clear = clarity >= 0.85
            print(f"  take {i + 1}: says {clarity:.0%} of the line; {label} {score:.2f} (sounds most like {top})", file=sys.stderr, flush=True)
            key = (clear, score if clear else clarity)
            if best is None or key > best[0]:
                best = (key, wav, engine, score, clarity)
            if clear and (judge is None or top == label):
                break
        if best and best[0][0]:
            break
        torch.cuda.empty_cache()
    _, wav, engine, score, clarity = best
    sf.write(job["out"], wav, RATE)
    print(json.dumps({"engine": engine, "accent": accent, "score": score, "clarity": clarity}))


if __name__ == "__main__":
    main()
