#!/usr/bin/env python3
"""Generate Utterly tray/pill assets with stdlib only (no Pillow).
Writes SVG sources + 16x16 PNGs (idle/listening/transcribing) + pill banner.
RAM/disk minimal: each PNG < 1 KiB."""
import struct, zlib, os

HERE = os.path.dirname(os.path.abspath(__file__))

SVG = {
    "idle": ("8E8E93", "mic idle — macOS grey"),
    "listening": ("FF453A", "mic listening — pulsing system red"),
    "transcribing": ("30D158", "mic transcribing — system green"),
}

def svg(name, color, desc):
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">
  <title>Utterly {name} — {desc}</title>
  <rect x="2" y="2" width="60" height="60" rx="14" fill="#1E1E20" stroke="#3A3A3C" stroke-width="2"/>
  <rect x="27" y="12" width="10" height="22" rx="5" fill="#{color}"/>
  <path d="M20 30 a12 12 0 0 0 24 0" fill="none" stroke="#{color}" stroke-width="3" stroke-linecap="round"/>
  <line x1="32" y1="42" x2="32" y2="50" stroke="#{color}" stroke-width="3" stroke-linecap="round"/>
  <line x1="25" y1="50" x2="39" y2="50" stroke="#{color}" stroke-width="3" stroke-linecap="round"/>
</svg>
"""

def png_circle(path, rgb):
    S = 16
    raw = b""
    for y in range(S):
        raw += b"\x00"
        for x in range(S):
            dx, dy = x - 8, y - 8
            d2 = dx*dx + dy*dy
            if d2 <= 30: a = 255
            elif d2 <= 42: a = 120
            else: a = 0
            r, g, b = rgb
            if a == 0: r = g = b = 0
            raw += struct.pack("BBBB", r, g, b, a)
    def chunk(typ, data):
        c = struct.pack(">I", len(data)) + typ + data
        return c + struct.pack(">I", zlib.crc32(typ + data) & 0xFFFFFFFF)
    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", struct.pack(">IIBBBBB", S, S, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(raw, 9))
           + chunk(b"IEND", b""))
    with open(path, "wb") as f:
        f.write(png)

os.makedirs(HERE, exist_ok=True)
colors = {"idle": (0x8E, 0x8E, 0x93), "listening": (0xFF, 0x45, 0x3A), "transcribing": (0x30, 0xD1, 0x58)}
for name, (color, desc) in SVG.items():
    with open(os.path.join(HERE, f"{name}.svg"), "w") as f:
        f.write(svg(name, color, desc))
    png_circle(os.path.join(HERE, f"{name}-16.png"), colors[name])
print("assets:", sorted(os.listdir(HERE)))
