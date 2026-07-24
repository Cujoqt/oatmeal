#!/usr/bin/env python3
"""Generate Oatmeal's app icon set from the waveform mark.

The mark is five rounded bars on a charcoal squircle — three cream, two rust —
rising and falling like a level meter. Everything is drawn here rather than
checked in as a binary so the icon can be re-tuned by editing numbers.

    python3 scripts/make-icons.py

Writes PNGs plus icon.icns into app/src-tauri/icons/.
"""

import pathlib
import shutil
import subprocess
import tempfile

from PIL import Image, ImageDraw

OUT = pathlib.Path(__file__).resolve().parent.parent / "app" / "src-tauri" / "icons"

# Drawn at 4x the largest export, then downsampled — cheap antialiasing that
# keeps the bar caps and the squircle corners clean at 32px.
CANVAS = 4096
SUPERSAMPLE = CANVAS // 1024

BG = (43, 40, 36, 255)  # charcoal
CREAM = (244, 238, 224, 255)
RUST = (184, 92, 57, 255)

# Fractions of the squircle's side.
PLATE = 0.805  # squircle side within the 1024 canvas (macOS icon padding)
CORNER = 0.225
BAR_W = 0.075
BAR_GAP = 0.055
# (height fraction, color) left to right — the rust pair carries the accent.
BARS = [
    (0.26, CREAM),
    (0.44, CREAM),
    (0.56, CREAM),
    (0.62, RUST),
    (0.32, RUST),
]

# Sizes Tauri's bundler and the .icns iconset need.
PNG_SIZES = {
    "32x32.png": 32,
    "128x128.png": 128,
    "128x128@2x.png": 256,
    "256x256.png": 256,
    "512x512.png": 512,
    "1024x1024.png": 1024,
    "icon.png": 512,
}
ICNS_SIZES = [
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
]


def render() -> Image.Image:
    img = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    plate = CANVAS * PLATE
    x0 = (CANVAS - plate) / 2
    y0 = (CANVAS - plate) / 2
    d.rounded_rectangle(
        [x0, y0, x0 + plate, y0 + plate],
        radius=plate * CORNER,
        fill=BG,
    )

    bar_w = plate * BAR_W
    gap = plate * BAR_GAP
    total = len(BARS) * bar_w + (len(BARS) - 1) * gap
    bx = (CANVAS - total) / 2
    cy = CANVAS / 2

    for frac, color in BARS:
        h = plate * frac
        d.rounded_rectangle(
            [bx, cy - h / 2, bx + bar_w, cy + h / 2],
            radius=bar_w / 2,
            fill=color,
        )
        bx += bar_w + gap

    return img


def main() -> None:
    master = render()
    OUT.mkdir(parents=True, exist_ok=True)

    for name, size in PNG_SIZES.items():
        master.resize((size, size), Image.LANCZOS).save(OUT / name)
        print(f"  {name} ({size}px)")

    with tempfile.TemporaryDirectory() as tmp:
        iconset = pathlib.Path(tmp) / "icon.iconset"
        iconset.mkdir()
        for name, size in ICNS_SIZES:
            master.resize((size, size), Image.LANCZOS).save(iconset / name)
        subprocess.run(
            ["iconutil", "-c", "icns", str(iconset), "-o", str(OUT / "icon.icns")],
            check=True,
        )
    print("  icon.icns")

    # The DMG background reuses the app icon; keep the copy next to it in sync
    # so a rebuilt bundle doesn't ship yesterday's mark.
    dmg_icon = OUT.parent / "target" / "release" / "bundle" / "dmg" / "icon.icns"
    if dmg_icon.exists():
        shutil.copy(OUT / "icon.icns", dmg_icon)


if __name__ == "__main__":
    main()
