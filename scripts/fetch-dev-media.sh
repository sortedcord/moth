#!/usr/bin/env bash
# Download one YouTube video once, in several codecs and resolutions, for moth's `local` dev source
# (see CONTRIBUTING.md). Afterwards `local:<name>` plays it from disk, without touching YouTube.
#
# Usage: scripts/fetch-dev-media.sh [video id] [name]
# Default: Spring (Blender Studio, CC BY) as `local:spring`. Needs yt-dlp and ffmpeg (thumbnail).
set -euo pipefail

id=${1:-WhWc3b3KhnY}
name=${2:-spring}
root="$(cd "$(dirname "$0")/.." && pwd)/target/dev-media"
dir="$root/$name"

# itags: VP9, AV1 and H.264 at 1080p and 480p; Opus and AAC audio.
formats=(248 244 399 397 137 135 251 140)

mkdir -p "$dir"
# Comma-separated formats are downloaded as separate files; files already present are skipped.
yt-dlp --no-warnings --no-part --no-mtime \
    -f "$(IFS=,; echo "${formats[*]}")" \
    -o "$dir/%(format_id)s.%(ext)s" \
    -o "infojson:$dir/info" --write-info-json \
    -o "thumbnail:$dir/thumbnail" --write-thumbnail --convert-thumbnails jpg \
    -- "$id"

# The manifest the local source reads (it's also easy to write by hand for other files).
python3 - "$dir" "${formats[@]}" <<'EOF'
import json, os, sys

dir, wanted = sys.argv[1], sys.argv[2:]
info = json.load(open(os.path.join(dir, "info.info.json")))
files = {f.split(".")[0]: f for f in os.listdir(dir)}

def video_codec(codec):
    for prefix, name in (("vp9", "vp9"), ("vp09", "vp9"), ("av01", "av1"), ("avc1", "h264")):
        if codec.startswith(prefix):
            return name

def audio_codec(codec):
    return "opus" if codec == "opus" else "aac" if codec.startswith("mp4a") else None

streams = []
for format in info["formats"]:
    id = format["format_id"]
    if id not in wanted or id not in files:
        continue
    stream = {"id": int(id), "file": files[id], "bitrate": round((format.get("tbr") or 0) * 1000) or None}
    if (format.get("vcodec") or "none") != "none":
        stream.update(kind="video", codec=video_codec(format["vcodec"]), width=format["width"],
                      height=format["height"], fps=round(format.get("fps") or 0), quality=format.get("format_note"))
    else:
        stream.update(kind="audio", codec=audio_codec(format["acodec"]))
    streams.append(stream)

manifest = {
    "title": info["title"],
    "channel": info.get("channel") or info.get("uploader"),
    "description": info.get("description") or "",
    "views": info.get("view_count"),
    "likes": info.get("like_count"),
    "upload_date": info.get("upload_date"),
    "duration": info.get("duration"),
    "thumbnail": "thumbnail.jpg" if os.path.exists(os.path.join(dir, "thumbnail.jpg")) else None,
    "streams": streams,
}
json.dump(manifest, open(os.path.join(dir, "manifest.json"), "w"), indent=2)
print(f"{dir}/manifest.json: {len(streams)} streams")
EOF
echo "Open it in moth as local:$name"
