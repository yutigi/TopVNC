"""Download the five FPS screenshots used in the foveated Tight R&D and write
each as 1920x1080 raw RGB (NAME.rgb) plus a PNG, for examples/fovea_latency.rs
and the R&D codec study (examples/fovea_study.rs in prototype.patch).

Usage: python3 fetch_frames.py OUT_DIR   (needs Pillow)
Sources are Wikimedia Commons; licenses: Xonotic GPL, Red Eclipse 2
CC BY-SA 4.0, Unvanquished CC BY-SA 2.5, AssaultCube public domain.
"""
import os, sys, urllib.request
from PIL import Image

FRAMES = {
    "xonotic": "https://upload.wikimedia.org/wikipedia/commons/b/b8/Xonotic_gameplay.png",
    "redeclipse_v2": "https://upload.wikimedia.org/wikipedia/commons/6/6d/Red_Eclipse_v2.0.0_Screenshot.png",
    "redeclipse_edge": "https://upload.wikimedia.org/wikipedia/commons/c/ce/Red_Eclipse_2_Screenshot_-_Edge.png",
    "unvanquished": "https://upload.wikimedia.org/wikipedia/commons/0/07/Unvanquished-0.52-human-builder.jpg",
    "assaultcube": "https://upload.wikimedia.org/wikipedia/commons/8/82/AssaultCube_screenshot.png",
}

out = sys.argv[1]
os.makedirs(out, exist_ok=True)
for name, url in FRAMES.items():
    source = os.path.join(out, name + os.path.splitext(url)[1])
    if not os.path.exists(source):
        request = urllib.request.Request(url, headers={"User-Agent": "TopVNC-RnD/0.1 (benchmark frames)"})
        with urllib.request.urlopen(request, timeout=60) as response, open(source, "wb") as f:
            f.write(response.read())
    image = Image.open(source).convert("RGB")
    if image.size != (1920, 1080):
        # Unvanquished is 2560x1440; AssaultCube 1645x972 (upscaled, slightly stretched).
        image = image.resize((1920, 1080), Image.LANCZOS)
    image.save(os.path.join(out, f"{name}_1080.png"))
    with open(os.path.join(out, f"{name}.rgb"), "wb") as f:
        f.write(image.tobytes())
    print(name, "ok")
