"""Additional public release/help references and inventory summaries."""
import collections
import hashlib
import html
import json
import pathlib
import re
import urllib.request
from concurrent.futures import ThreadPoolExecutor

OUT = pathlib.Path(__file__).resolve().parents[1] / "docs/references"
urls = ["https://willow-electron-builds.s3.us-east-1.amazonaws.com/public/windows-x64/latest.yml", "https://willow-electron-builds.s3.us-east-1.amazonaws.com/public/latest/willow-voice-installer.exe", "https://help.willowvoice.com/en/"]
records = []
for url in urls:
    try:
        req = urllib.request.Request(url, method="HEAD" if url.endswith(".exe") else "GET", headers={"User-Agent": "Mozilla/5.0"})
        with urllib.request.urlopen(req, timeout=20) as r:
            records.append({"url": url, "headers": dict(r.headers), "final_url": r.url})
            if not url.endswith(".exe"):
                (OUT / "web" / ("help.html" if url.endswith("/en/") else "latest.yml")).write_bytes(r.read())
    except Exception as e:
        records.append({"url": url, "error": str(e)})
(OUT / "web/release-and-help.json").write_text(json.dumps(records, indent=2), encoding="utf-8")
print(json.dumps(records, indent=2))
def help_page(url):
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
        with urllib.request.urlopen(req, timeout=20) as r:
            data = r.read()
        p = OUT / "web" / (url.rsplit("/", 1)[-1] + ".html")
        p.write_bytes(data)
        return {"source": url, "path": p.name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
    except Exception as e:
        return {"source": url, "error": str(e)}
help_records = []
with ThreadPoolExecutor(max_workers=8) as pool:
    help_records += list(pool.map(help_page, ["https://help.willowvoice.com/en/collections/12093043-getting-started", "https://help.willowvoice.com/en/collections/12093044-troubleshooting-and-faqs"]))
    articles = set()
    for record in help_records:
        if "path" in record:
            articles.update(re.findall(r'https://help.willowvoice.com/en/articles/[a-z0-9-]+', (OUT / "web" / record["path"]).read_text(encoding="utf-8")))
    help_records += list(pool.map(help_page, sorted(articles)))
(OUT / "web/help-manifest.json").write_text(json.dumps(help_records, indent=2), encoding="utf-8")
# Search results scoped to public official pages; retain readable copy without scripts/styles.
pages = {}
for p in (OUT / "web").glob("*.html"):
    s = p.read_text(encoding="utf-8")
    s = re.sub(r'<(script|style)\b[^>]*>.*?</\1>', '', s, flags=re.S|re.I)
    s = html.unescape(re.sub('<[^>]+>', ' ', s))
    pages[p.name] = re.sub(r'\s+', ' ', s).strip()
(OUT / "web/page-text.json").write_text(json.dumps(pages, indent=2, ensure_ascii=False), encoding="utf-8")
print("INSTALLED", dict(collections.Counter(p.suffix for p in (OUT / "installed").rglob("*") if p.is_file())))
print("WEB", dict(collections.Counter(p.suffix for p in (OUT / "web").rglob("*") if p.is_file())))
