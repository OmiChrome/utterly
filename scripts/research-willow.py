"""Read-only Willow reference collection. Never accesses user profile/application data."""
import hashlib
import json
import pathlib
import shutil
import struct
import urllib.request
from datetime import datetime, timezone

ROOT = pathlib.Path(__file__).resolve().parents[1]
OUT = ROOT / "docs/references"
INSTALL = pathlib.Path("C:/Users/omi/AppData/Local/Programs/Willow Voice")
ASAR = INSTALL / "resources/app.asar"
manifest = []


def save(data, destination, source):
    path = OUT / destination
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    manifest.append({"path": str(path.relative_to(ROOT)).replace("\\", "/"), "source": source,
                     "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})


def entries(tree, prefix=""):
    for name, value in tree["files"].items():
        path = f"{prefix}/{name}".lstrip("/")
        if "files" in value:
            yield from entries(value, path)
        else:
            yield path, value


def collect():
    screenshots = pathlib.Path("C:/Users/omi/Pictures/Screenshots")
    names = ["Screenshot 2026-09-24 004525.png", "Screenshot 2026-09-24 004345.png", "Screenshot (39).png",
             "Screenshot 2026-09-24 004156.png", "Screenshot 2026-09-24 004117.png", "Screenshot 2026-09-24 004042.png"]
    clips = ["8ea5689c-115a-4f0a-bd33-800e1015b92d", "a28fcfcc-539d-4674-8eb2-024e1de55e01", "bdcbe4c1-f2f5-4640-9941-f381f080d423", "6725f020-2579-4983-81a3-a7227b40f477", "d7ad75a5-7171-44d2-8306-3eb550793b2f"]
    sources = [screenshots / n for n in names] + [pathlib.Path(f"C:/Users/omi/AppData/Local/Temp/codex-clipboard-{c}.png") for c in clips]
    sources.append(pathlib.Path("C:/Users/omi/Videos/Recording 2026-09-24 020729.mp4"))
    for src in sources:
        save(src.read_bytes(), pathlib.Path("supplied") / src.name, str(src))

    with ASAR.open("rb") as f:
        first = f.read(16)
        _, header_size, _, json_size = struct.unpack("<4I", first)
        index = json.loads(f.read(json_size))
        offset = 8 + header_size
        asset_ext = {".png", ".svg", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".icns", ".woff", ".woff2", ".ttf", ".otf", ".wav", ".mp3", ".ogg", ".m4a", ".webm", ".mp4", ".lottie"}
        listing = []
        for name, meta in entries(index):
            listing.append({"path": name, "size": meta.get("size"), "unpacked": meta.get("unpacked", False)})
            ext = pathlib.PurePosixPath(name).suffix.lower()
            # App code is retained only as a local inspection reference; no dependencies copied.
            selected = name == "package.json" or name.startswith("out/") and (ext in asset_ext or ext in {".css", ".html", ".js"})
            if not selected or meta.get("link"):
                continue
            if meta.get("unpacked"):
                data = (pathlib.Path(str(ASAR) + ".unpacked") / name).read_bytes()
            else:
                f.seek(offset + int(meta["offset"]))
                data = f.read(meta["size"])
                assert len(data) == meta["size"], name
            save(data, pathlib.Path("installed") / name, f"{ASAR}!/{name}")
    save(json.dumps(listing, indent=2).encode(), "installed/asar-index.json", str(ASAR))
    for src in (INSTALL / "resources/tray").rglob("*"):
        if src.is_file():
            save(src.read_bytes(), pathlib.Path("installed/tray") / src.relative_to(INSTALL / "resources/tray"), str(src))
    files = [p for p in INSTALL.rglob("*") if p.is_file()]
    save(json.dumps({"observed_utc": datetime.now(timezone.utc).isoformat(), "files": len(files), "bytes": sum(p.stat().st_size for p in files), "largest": sorted([{"path": str(p.relative_to(INSTALL)), "bytes": p.stat().st_size} for p in files], key=lambda x: -x["bytes"])[:20]}, indent=2).encode(), "installed/size.json", str(INSTALL))
    for name, url in [("homepage.html", "https://willowvoice.com/"), ("sitemap.xml", "https://willowvoice.com/sitemap.xml"), ("robots.txt", "https://willowvoice.com/robots.txt")]:
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
            with urllib.request.urlopen(req, timeout=30) as response:
                save(response.read(), pathlib.Path("web") / name, url)
        except Exception as e:
            print(url, str(e))
    save(json.dumps(manifest, indent=2).encode(), "manifest.json", "Collection manifest; assets remain reference-only")
    print(f"Collected {len(manifest)} references into {OUT}")


if __name__ == "__main__":
    collect()
