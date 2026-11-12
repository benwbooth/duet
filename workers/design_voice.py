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
    ("british", r"british|english|englishm[ae]n|englishwom[ae]n|(?<!new )england|received pronunciation|\brp\b|london|cockney|oxford|yorkshire|bbc"),
    ("american", r"american|united states|new york|new england|\bmaine\b|texan|southern drawl|\bus\b"),
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


WORD_NUMBERS = {w: i for i, w in enumerate(
    "zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen "
    "sixteen seventeen eighteen nineteen".split())}
WORD_NUMBERS.update({w: 10 * i for i, w in enumerate("twenty thirty forty fifty sixty seventy eighty ninety".split(), 2)})
NUMBER = r"(\d{1,2}|" + "|".join(WORD_NUMBERS) + r")"


def age_from_years(n):
    return "child" if n <= 12 else "teenager" if n <= 19 else "young adult" if n < 40 else "middle-aged" if n < 60 else "elderly"


def age_of(text):
    """OmniVoice's age for a description. The director starts with the age ("A male voice, a
    boy of 11." / "A female voice, about 50."), so that's read first, and a number only counts
    as an age where it reads as one: a "mother of four" or a teacher "of children" isn't a child."""
    head = re.split(r"\.\s", text, maxsplit=1)[0]
    for t in (head, text):
        decade = re.search(r"\b(?:early|mid|late)?[- ]?([1-9])0s\b", t)
        years = re.search(
            rf"\b(?:about|aged|age|around|roughly|maybe|perhaps|voice,)\s+{NUMBER}\b|\b{NUMBER}[- ]years?[- ]old\b|"
            rf"\b(?:boys?|girls?|child|children|kids?|toddler|baby|son|daughter|teen|teenager)\s+(?:of\s+)?(?:about\s+)?{NUMBER}\b",
            t,
        )
        if years:
            w = next(g for g in years.groups() if g)
            return age_from_years(int(w) if w.isdigit() else WORD_NUMBERS[w])
        if decade:
            return age_from_years(int(decade.group(1)) * 10)
        if re.search(r"\b(child|kid|toddler|baby|little girl|little boy|young girl|young boy|pre-pubescent|schoolgirl|schoolboy|boys?|girls?)\b", t):
            return "child"
        if re.search(r"\b(teen|teens|teenage|teenager|adolescent)\b", t):
            return "teenager"
        if re.search(r"\b(middle-aged|middle aged|forties|fifties)\b", t):
            return "middle-aged"
        if re.search(r"\b(elderly|old man|old woman|ancient|sixties|seventies|eighties)\b", t):
            return "elderly"
        if re.search(r"\b(young|twenties|thirties)\b", t):
            return "young adult"
    return None


# Children's voices sit around 220-300 Hz; a man's is about 85-155 and a woman's 165-255.
# A child's take below CHILD_HZ is a grown-up's voice, whatever was asked for. One far above
# CHILD_MAX_HZ is a squeak Fish can't copy: George's 449 Hz clip came out of Fish as a man's
# mumble at 70-90 Hz.
CHILD_HZ, CHILD_MAX_HZ, CHILD_TYPICAL_HZ = 190, 340, 260


def pitch_hz(wav):
    """The median pitch of a take's voiced speech, in Hz."""
    import librosa

    y = librosa.resample(np.asarray(wav, dtype=np.float32), orig_sr=RATE, target_sr=16000)
    f0, voiced, _ = librosa.pyin(y, fmin=60, fmax=600, sr=16000)
    f0 = f0[voiced & np.isfinite(f0)]
    return float(np.median(f0)) if f0.size else 0.0


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


_omni = []


def omni(text, instruct):
    from omnivoice import OmniVoice

    if not _omni:  # loaded once for all the rounds
        _omni.append(OmniVoice.from_pretrained("k2-fsa/OmniVoice", device_map="cuda:0", dtype=torch.float16))
    m = _omni[0]
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

    # Rounds of takes. OmniVoice often answers "male, child" with a grown man's voice (Bill,
    # 11, came out at 86 Hz), and Fish then plays the boy as a man, so a child's take must
    # also sound like a child: its pitch is measured, and a round that only makes men is
    # followed by one that doesn't name the sex (a child's voice is a child's voice before it
    # breaks), then one asking for a girl's voice, the most reliable child OmniVoice makes.
    # A child that won't come out clear at all is tried again a little older.
    # (engine, prompt, takes, lowest pitch a take may have in Hz; negative: a last try, only
    # when no take so far has said its line clearly)
    rounds = []
    if omni_can:
        sex, age, pitch = sex_of(low), age_of(low), pitch_of(low)
        acc = [f"{accent} accent"] if accent else []
        if age == "child":
            # A deep or husky boy is still a boy: "low pitch" only pulls OmniVoice to men.
            up = [pitch] if pitch in ("high pitch", "very high pitch") else []
            rounds.append(("omnivoice", ", ".join([x for x in (sex, "child") if x] + up + acc), 4, CHILD_HZ))
            rounds.append(("omnivoice", ", ".join(["child", "high pitch"] + acc), 4, CHILD_HZ))
            if sex == "male":
                rounds.append(("omnivoice", ", ".join(["female", "child"] + acc), 3, CHILD_HZ))
            rounds.append(("omnivoice", ", ".join([x for x in (sex, "teenager", pitch) if x] + acc), 3, -CHILD_HZ))
        else:
            rounds.append(("omnivoice", ", ".join([x for x in (sex, age, pitch) if x] + acc), 4, 0))
    else:
        rounds.append(("maya1", desc, 3, CHILD_HZ if age_of(low) == "child" else 0))
    best = None  # (key, wav, engine, accent score, clarity)
    for engine, prompt, tries, min_hz in rounds:
        if min_hz < 0:
            if best and best[0][1]:
                break
            min_hz = -min_hz
        print(f"  {engine}: {prompt[:120]}", file=sys.stderr, flush=True)
        make = omni(text, prompt) if engine == "omnivoice" else maya(text, prompt)
        for i in range(tries):
            wav = make()
            clarity = ears.clarity(wav, text) if ears else 1.0
            score, top = judge.score(wav, label) if judge else (0.0, None)
            hz = pitch_hz(wav) if min_hz else 0.0
            clear, young = clarity >= 0.85, not min_hz or min_hz <= hz <= CHILD_MAX_HZ
            pitch_note = f"; pitch {hz:.0f} Hz" + ("" if young else f" (a child is {min_hz}-{CHILD_MAX_HZ})") if min_hz else ""
            print(f"  take {i + 1}: says {clarity:.0%} of the line; {label} {score:.2f} (sounds most like {top}){pitch_note}", file=sys.stderr, flush=True)
            # Clear and the right age first; then clear and nearest a child's pitch; then clearest.
            key = (clear and young, clear, score if clear and young else (-abs(hz - CHILD_TYPICAL_HZ) if clear else clarity))
            if best is None or key > best[0]:
                best = (key, wav, engine, score, clarity)
            if clear and young and (judge is None or top == label):
                break
        if best and best[0][0]:
            break
        torch.cuda.empty_cache()
    _, wav, engine, score, clarity = best
    sf.write(job["out"], wav, RATE)
    print(json.dumps({"engine": engine, "accent": accent, "score": score, "clarity": clarity}))


if __name__ == "__main__":
    main()
