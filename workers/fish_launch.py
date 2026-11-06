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

script = sys.argv[1]
sys.argv = sys.argv[1:]
sys.path[0] = os.path.dirname(os.path.abspath(script))
runpy.run_path(script, run_name="__main__")
