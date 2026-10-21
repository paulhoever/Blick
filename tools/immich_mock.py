#!/usr/bin/env python3
"""Immich-Attrappe für Tests von Blick. Nie gegen ein echtes Immich-Konto testen.

    python3 tools/immich_mock.py <bilderordner> [port]

Lauscht auf 127.0.0.1 (Standard 2291), API-Schlüssel `demo-key`.
Bildet die Endpunkte nach, die Blick nutzt, in den Formaten der spec.json von
Immich 3.3.1: Zeitleiste spaltenweise, Upload als multipart mit Prüfsumme.
Jede Anfrage landet auf stdout, so lässt sich prüfen, was die App gefragt hat.
"""
import io
import json
import os
import subprocess
import sys
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

from PIL import Image

# HEIC nur, wenn pillow-heif da ist (pip install pillow-heif).
try:
    import pillow_heif
    pillow_heif.register_heif_opener()
    HEIF = (".heic", ".heif")
except ImportError:
    HEIF = ()

KEY = os.environ.get("MOCK_KEY", "demo-key")
SRC = sys.argv[1] if len(sys.argv) > 1 else "."
VIDEO = (".mp4", ".mov", ".webm", ".mkv")
FILES = sorted(f for f in os.listdir(SRC) if f.lower().endswith((".jpg", ".jpeg", ".png") + HEIF + VIDEO))
# Was Browser direkt zeigen können; nur dafür leitet `size=fullsize` aufs Original um.
WEB = (".jpg", ".jpeg", ".png", ".webp", ".gif", ".avif")

def video_frame(path):
    """Standbild als JPEG-Bytes (ffmpeg), wie Immichs Video-Thumbnail."""
    return subprocess.run(["ffmpeg", "-v", "error", "-ss", "0.5", "-i", path, "-frames:v", "1",
                           "-f", "image2pipe", "-vcodec", "mjpeg", "-"], capture_output=True, check=True).stdout


ASSETS = []
for i, name in enumerate(FILES):
    month = "2026-10-01" if i % 2 == 0 else "2026-08-01"
    video = name.lower().endswith(VIDEO)
    src = io.BytesIO(video_frame(os.path.join(SRC, name))) if video else os.path.join(SRC, name)
    with Image.open(src) as im:
        ratio = im.width / im.height
    ASSETS.append({"id": f"a{i}", "file": name, "month": month, "ratio": ratio, "video": video,
                   "created": f"{month[:8]}{1 + i % 28:02d}T09:00:00.000Z"})
ALBUMS = {"al1": {"name": "Garten", "ids": [a["id"] for a in ASSETS[::2]]},
          "al2": {"name": "Urlaub 2026", "ids": [a["id"] for a in ASSETS[1::2]]}}
CHECKSUMS = {}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        print("REQ", self.command, self.path, flush=True)

    def send(self, code, body, ctype="application/json"):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def body(self):
        return self.rfile.read(int(self.headers.get("Content-Length", 0)))

    def authed(self):
        # Wie Immich: Schlüssel im Header oder als Query `apiKey` (GStreamer kann keine Header).
        q = parse_qs(urlparse(self.path).query)
        if self.headers.get("x-api-key") == KEY or q.get("apiKey", [None])[0] == KEY:
            return True
        self.send(401, {"message": "Invalid API key", "statusCode": 401})
        return False

    def do_POST(self):
        if not self.authed():
            return
        if self.path == "/api/shared-links":
            body = json.loads(self.body() or b"{}")
            print("SHARE", body.get("assetIds"), flush=True)
            return self.send(201, {"id": str(uuid.uuid4()), "key": uuid.uuid4().hex})
        if self.path != "/api/assets":
            return self.send(404, {})
        raw = self.body()
        if b'name="assetData"' not in raw or b'name="fileCreatedAt"' not in raw:
            return self.send(400, {"message": "assetData oder fileCreatedAt fehlt"})
        checksum = self.headers.get("x-immich-checksum", "")
        if checksum in CHECKSUMS:
            print("UPLOAD duplicate", checksum, flush=True)
            return self.send(200, {"id": CHECKSUMS[checksum], "status": "duplicate"})
        new_id = str(uuid.uuid4())
        CHECKSUMS[checksum] = new_id
        print("UPLOAD created", new_id, len(raw), "bytes", flush=True)
        self.send(201, {"id": new_id, "status": "created"})

    def do_PUT(self):
        if not self.authed():
            return
        parts = urlparse(self.path).path.strip("/").split("/")
        if len(parts) == 4 and parts[:2] == ["api", "albums"] and parts[3] == "assets":
            ids = json.loads(self.body() or b"{}").get("ids", [])
            album = ALBUMS.get(parts[2])
            if album is None:
                return self.send(404, {"message": "Album nicht gefunden"})
            out = []
            for i in ids:
                ok = i not in album["ids"]
                if ok:
                    album["ids"].append(i)
                out.append({"id": i, "success": ok, **({} if ok else {"error": "duplicate"})})
            print("ALBUM", parts[2], ids, flush=True)
            return self.send(200, out)
        self.send(404, {})

    def do_GET(self):
        url = urlparse(self.path)
        q = {k: v[0] for k, v in parse_qs(url.query).items()}
        if url.path == "/api/server/ping":
            return self.send(200, {"res": "pong"})
        if not self.authed():
            return
        parts = url.path.strip("/").split("/")
        if url.path == "/api/server/config":
            return self.send(200, {"externalDomain": ""})
        if url.path == "/api/users/me":
            return self.send(200, {"id": "u1", "name": "Demo", "email": "demo@example.org", "profileImagePath": "",
                                   "avatarColor": "primary", "profileChangedAt": "2026-01-01T00:00:00.000Z"})
        if url.path == "/api/albums":
            return self.send(200, [{"id": k, "albumName": v["name"], "description": "", "assetCount": len(v["ids"]),
                                    "albumThumbnailAssetId": v["ids"][0] if v["ids"] else None, "shared": k == "al2",
                                    "albumUsers": [], "hasSharedLink": False, "isActivityEnabled": False,
                                    "createdAt": "2026-09-01T00:00:00.000Z", "updatedAt": "2026-09-01T00:00:00.000Z"}
                                   for k, v in ALBUMS.items()])
        pool = [a for a in ASSETS if "albumId" not in q or a["id"] in ALBUMS.get(q["albumId"], {}).get("ids", [])]
        if url.path == "/api/timeline/buckets":
            months = sorted({a["month"] for a in pool}, reverse=True)
            return self.send(200, [{"timeBucket": m, "count": sum(a["month"] == m for a in pool)} for m in months])
        if url.path == "/api/timeline/bucket":
            items = [a for a in pool if a["month"] == q.get("timeBucket")]
            n = len(items)
            return self.send(200, {
                "id": [a["id"] for a in items], "ownerId": ["u1"] * n, "ratio": [a["ratio"] for a in items],
                "isFavorite": [False] * n, "visibility": ["timeline"] * n, "isTrashed": [False] * n,
                "isImage": [not a["video"] for a in items], "thumbhash": [None] * n,
                "fileCreatedAt": [a["created"] for a in items], "createdAt": [a["created"] for a in items],
                "localOffsetHours": [2] * n, "duration": ["0:00:05.000000" if a["video"] else None for a in items], "projectionType": [None] * n,
                "livePhotoVideoId": [None] * n})
        if len(parts) == 3 and parts[:2] == ["api", "assets"]:
            a = next((x for x in ASSETS if x["id"] == parts[2]), None)
            return self.send(200, {"id": a["id"], "originalFileName": a["file"]}) if a else self.send(404, {})
        if len(parts) == 4 and parts[:2] == ["api", "assets"] and parts[3] == "original":
            a = next((x for x in ASSETS if x["id"] == parts[2]), None)
            if a is None:
                return self.send(404, {})
            return self.send(200, open(os.path.join(SRC, a["file"]), "rb").read(), "application/octet-stream")
        if len(parts) == 5 and parts[:2] == ["api", "assets"] and parts[3:] == ["video", "playback"]:
            a = next((x for x in ASSETS if x["id"] == parts[2] and x["video"]), None)
            if a is None:
                return self.send(404, {})
            data = open(os.path.join(SRC, a["file"]), "rb").read()
            rng = self.headers.get("Range", "")
            if rng.startswith("bytes="):
                start, _, end = rng[6:].partition("-")
                start, end = int(start or 0), int(end) if end else len(data) - 1
                part = data[start:end + 1]
                self.send_response(206)
                self.send_header("Content-Range", f"bytes {start}-{start + len(part) - 1}/{len(data)}")
            else:
                part = data
                self.send_response(200)
            self.send_header("Content-Type", "video/mp4")
            self.send_header("Accept-Ranges", "bytes")
            self.send_header("Content-Length", str(len(part)))
            self.end_headers()
            self.wfile.write(part)
            return
        if len(parts) == 4 and parts[:2] == ["api", "assets"] and parts[3] == "thumbnail":
            a = next((x for x in ASSETS if x["id"] == parts[2]), None)
            if a is None:
                return self.send(404, {})
            # Immich 3.3.1 ohne erzeugte Fullsize-Dateien: Web-Formate gehen
            # aufs Original, alles andere (HEIC, RAW) auf die Vorschau.
            if q.get("size") == "fullsize":
                web = a["file"].lower().endswith(WEB)
                self.send_response(302)
                self.send_header("Location", "original" if web else "thumbnail?size=preview")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            px = 1440 if q.get("size") == "preview" else 250
            src = io.BytesIO(video_frame(os.path.join(SRC, a["file"]))) if a["video"] else os.path.join(SRC, a["file"])
            with Image.open(src) as im:
                im = im.convert("RGB")
                im.thumbnail((px, px))
                out = io.BytesIO()
                im.save(out, "JPEG", quality=80)
            return self.send(200, out.getvalue(), "image/jpeg")
        self.send(404, {})


if __name__ == "__main__":
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 2291
    print(f"Immich-Attrappe auf 127.0.0.1:{port}, {len(ASSETS)} Bilder, Schlüssel {KEY}", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
