"""Runs a fish-speech script with TensorBoard stubbed out and audio loaded with soundfile.

fish-speech pulls in TensorBoard (through audiotools) only for training logs, and some TensorBoard
builds don't load next to fish-speech's protobuf. Serving speech never writes a log, so a stand-in
is enough and the server starts whatever TensorBoard is installed.

    python fish_launch.py tools/api_server.py [args...]
"""

import os
import runpy
import sys
import types


class SummaryWriter:
    def __init__(self, *args, **kwargs):
        pass

    def __getattr__(self, name):
        return lambda *args, **kwargs: None


stub = types.ModuleType("torch.utils.tensorboard")
stub.SummaryWriter = SummaryWriter
sys.modules["torch.utils.tensorboard"] = stub



def _load(source, *args, **kwargs):
    """torchaudio.load through soundfile: newer torchaudio needs TorchCodec for this, which
    isn't built for every GPU platform."""
    import soundfile as sf
    import torch

    data, sr = sf.read(source, dtype="float32", always_2d=True)
    return torch.from_numpy(data.T.copy()), sr


try:
    import torchaudio

    torchaudio.load = _load
except ImportError:
    pass

# On AMD GPUs MIOpen searches for the best convolution kernel for every new input length, which
# makes decoding a sentence take far longer than generating it. PyTorch's own kernels need no search.
try:
    import torch

    if torch.version.hip and os.environ.get("DUET_MIOPEN") != "1":
        torch.backends.cudnn.enabled = False
except ImportError:
    pass

_max_seq = int(os.environ.get("DUET_FISH_MAX_SEQ", "4096"))

# How fish-speech's decode step is compiled can be overridden, to tune it for the GPU at hand:
# DUET_FISH_COMPILE="reduce-overhead" (CUDA/HIP graphs: far fewer kernel launches per frame) and
# DUET_FISH_DYNAMIC=0 (static shapes, which graphs need).
_mode = os.environ.get("DUET_FISH_COMPILE")
_dynamic = os.environ.get("DUET_FISH_DYNAMIC")
if _mode or _dynamic is not None:
    try:
        import torch

        _compile = torch.compile

        _bucket = int(os.environ.get("DUET_FISH_KV_BUCKET", "0"))

        def compile(fn=None, *args, **kwargs):
            if _mode:
                kwargs["mode"] = _mode
            if _dynamic is not None:
                kwargs["dynamic"] = _dynamic == "1"
            compiled = _compile(fn, *args, **kwargs) if fn is not None else _compile(*args, **kwargs)
            if not _bucket or fn is None:
                return compiled

            # The key/value length grows by one every frame; graphs are recorded per length, so
            # it's rounded up to a bucket (the positions past the current one are masked anyway).
            def bucketed(*a, **kw):
                if kw.get("kv_len") is not None:
                    kw["kv_len"] = min(_max_seq, -(-kw["kv_len"] // _bucket) * _bucket)
                return compiled(*a, **kw)

            return bucketed

        torch.compile = compile
    except ImportError:
        pass

def _int8(model):
    """Every big linear layer holds its weights as 8-bit integers with a scale per output
    channel (DUET_FISH_INT8=1). Generating speech reads every weight for every frame, so half the
    bytes is close to twice the frames; compiled, the scaling folds into the matrix multiply."""
    import torch
    from torch import nn

    class Int8Linear(nn.Module):
        def __init__(self, linear):
            super().__init__()
            w = linear.weight.data.float()
            scale = w.abs().amax(dim=1).clamp(min=1e-8) / 127.0
            self.register_buffer("weight", torch.round(w / scale[:, None]).clamp(-127, 127).to(torch.int8))
            self.register_buffer("scale", scale.to(linear.weight.dtype))
            self.bias = linear.bias
            self.in_features, self.out_features = linear.in_features, linear.out_features

        def forward(self, x):
            out = nn.functional.linear(x, self.weight.to(x.dtype)) * self.scale.to(x.dtype)
            return out + self.bias if self.bias is not None else out

    count = 0
    for name, module in list(model.named_modules()):
        for child_name, child in list(module.named_children()):
            # (The big matrices in the transformer layers; embeddings and the output head stay.)
            if isinstance(child, nn.Linear) and child.weight.numel() >= 1 << 20 and "output" not in child_name:
                setattr(module, child_name, Int8Linear(child))
                count += 1
    print(f"fish_launch: {count} layers in 8-bit", file=sys.stderr)


# S2-Pro's config allows 32,768 tokens of context, and fish-speech sizes its key/value cache and
# attention mask for all of it: gigabytes of memory, and every generated frame attends over the
# whole length. A line of a book plus its voice reference needs a small fraction of that.
if _max_seq > 0:
    try:
        sys.path.insert(0, os.getcwd())
        from fish_speech.models.text2semantic import llama as _llama

        _owner = next(c for c in _llama.DualARTransformer.__mro__ if "from_pretrained" in c.__dict__)
        _from_pretrained = _owner.__dict__["from_pretrained"].__func__

        def _capped(path, *args, **kwargs):
            if kwargs.get("max_length") is None:
                kwargs["max_length"] = _max_seq
            model = _from_pretrained(path, *args, **kwargs)
            if os.environ.get("DUET_FISH_INT8") == "1":
                _int8(model)
            return model

        _owner.from_pretrained = staticmethod(_capped)
    except Exception as e:  # an older or newer fish-speech: run it as it is
        print(f"fish_launch: couldn't cap the context length ({e})", file=sys.stderr)

script = sys.argv[1]
sys.argv = sys.argv[1:]
sys.path[0] = os.path.dirname(os.path.abspath(script))
runpy.run_path(script, run_name="__main__")
