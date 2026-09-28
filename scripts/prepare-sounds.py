"""Generate short original start/stop recording cues with the Python stdlib."""
import math
import struct
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RATE = 22_050
DURATION = 0.09
AMPLITUDE = 0.075


def write_cue(path: Path, frequencies: tuple[int, int]) -> None:
    frames = round(RATE * DURATION)
    samples = bytearray()
    for i in range(frames):
        t = i / RATE
        attack = min(1.0, t / 0.008)
        release = min(1.0, (DURATION - t) / 0.025)
        envelope = min(attack, release)
        value = sum(math.sin(2 * math.pi * f * t) for f in frequencies) / 2
        sample = round(32_767 * AMPLITUDE * envelope * value)
        samples.extend(struct.pack("<h", sample))

    with wave.open(str(path), "wb") as output:
        output.setnchannels(1)
        output.setsampwidth(2)
        output.setframerate(RATE)
        output.writeframes(samples)


def check_cue(path: Path) -> None:
    with wave.open(str(path), "rb") as source:
        assert source.getnchannels() == 1
        assert source.getsampwidth() == 2
        assert source.getframerate() == RATE
        assert source.getnframes() == round(RATE * DURATION)
        assert path.stat().st_size < 20_000


if __name__ == "__main__":
    assets = ROOT / "assets"
    assets.mkdir(exist_ok=True)
    cues = {
        "record-start.wav": (660, 880),
        "record-stop.wav": (440, 660),
    }
    for name, frequencies in cues.items():
        path = assets / name
        write_cue(path, frequencies)
        check_cue(path)
        print(f"{path.name}: {path.stat().st_size} bytes, mono 22050 Hz, 16-bit PCM, 90 ms")
