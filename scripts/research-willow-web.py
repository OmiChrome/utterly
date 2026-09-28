"""Public website reference collector and compact packaged-code observations."""
import base64
import concurrent.futures
import hashlib
import html
import json
import pathlib
import re
import urllib.parse
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
OUT = ROOT / "docs/references"


def fetch(url):
    try:
        request = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
        with urllib.request.urlopen(request, timeout=25) as response:
            data = response.read(12_000_001)
            if len(data) > 12_000_000:
                return {"source": url, "error": "Reference exceeded 12 MB collection cap"}
            name = urllib.parse.urlparse(url).path.rstrip("/").split("/")[-1] or "homepage.html"
            if not pathlib.Path(name).suffix:
                name += ".html"
            destination = OUT / "web" / (hashlib.sha256(url.encode()).hexdigest()[:10] + "-" + name)
            destination.write_bytes(data)
            return {"source": url, "path": str(destination.relative_to(ROOT)).replace("\\", "/"), "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
    except Exception as error:
        return {"source": url, "error": str(error)}


def main():
    home = (OUT / "web/homepage.html").read_text(encoding="utf-8")
    urls = sorted(set(html.unescape(x) for x in re.findall(r'https?[^\s"<>]+', home)))
    links = sorted(set(html.unescape(x) for x in re.findall(r'href="([^"]+)"', home)))
    print("PUBLIC LINKS", "\n".join(links))
    assets = [u for u in urls if re.search(r'\.(?:png|svg|jpg|jpeg|webp|woff2?|css)(?:\?|$)', u)]
    # URL() font sources sometimes end in closing punctuation.
    assets += [u.rstrip(");'") for u in urls if "fonts.gstatic.com" in u]
    pages = ["https://willowvoice.com/" + p for p in ["download", "use-cases/windows", "pricing", "privacy-policy", "blog"]]
    pages += [u for u in links if "help." in u or "support." in u]
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        records = list(pool.map(fetch, sorted(set(assets + pages))))
    # Collect inline media from installed, static app bundles (no account data).
    seen = set()
    for source in (OUT / "installed/out").rglob("*"):
        if source.suffix not in {".js", ".css", ".html"}:
            continue
        text = source.read_text(encoding="utf-8")
        for mime, encoded in re.findall(r'data:((?:image|audio)/[\w.+-]+);base64,([A-Za-z0-9+/=]+)', text):
            data = base64.b64decode(encoded)
            digest = hashlib.sha256(data).hexdigest()
            if digest in seen:
                continue
            seen.add(digest)
            ext = {"svg+xml": "svg", "mpeg": "mp3", "x-icon": "ico"}.get(mime.split("/")[1], mime.split("/")[1])
            path = OUT / "installed/embedded" / (digest[:12] + "." + ext)
            path.parent.mkdir(exist_ok=True)
            path.write_bytes(data)
            records.append({"source": str(source.relative_to(ROOT)) + " inline data URL", "path": str(path.relative_to(ROOT)).replace("\\", "/"), "bytes": len(data), "sha256": digest})
    (OUT / "web-and-embedded-manifest.json").write_text(json.dumps(records, indent=2), encoding="utf-8")
    s = (OUT / "installed/out/main/index.js").read_text(encoding="utf-8")
    snippets = []
    for term in [r'W=\d+', r'U=\d+', r'B=\d+', r'Mi=\d+', r'Oi=\d+', r'Wi=\d+', "displayChangeDebounceTimer=setTimeout", "minWidth", "computeOverlayBounds"]:
        for match in list(re.finditer(term, s))[:2]:
            snippets.append(s[max(0, match.start()-150):match.end()+400])
    settings = (OUT / "installed/out/renderer/assets/SettingsModal-Bdzp-61Q.js").read_text(encoding="utf-8")
    snippets += [settings[max(0, m.start()-100):m.start()+1200] for m in re.finditer(r'max-w-|min-w-|w-\[|rounded-\[|grid-cols-', settings)]
    (OUT / "installed/layout-snippets.txt").write_text("\n\n".join(snippets), encoding="utf-8")
    print(f"Web/inline collected {sum('path' in r for r in records)}, failures {sum('error' in r for r in records)}")


if __name__ == "__main__":
    main()
