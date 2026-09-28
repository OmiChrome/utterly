"""Generate native runtime artwork from the user-selected icon.png."""
from pathlib import Path
from PIL import Image

root = Path(__file__).resolve().parents[1]
with Image.open(root / "icon.png") as source:
    art = source.convert("RGBA")
    art.save(root / "assets/utterly-app-icon.png")
    art.resize((32, 32), Image.Resampling.LANCZOS).tobytes()
    (root / "assets/utterly-tray-32.rgba").write_bytes(
        art.resize((32, 32), Image.Resampling.LANCZOS).tobytes()
    )
    art.save(root / "assets/utterly.ico", sizes=[(16,16), (32,32), (48,48), (64,64), (256,256)])
