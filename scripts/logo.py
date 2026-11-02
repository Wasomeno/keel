#!/usr/bin/env python3
"""Builds macOS app icons for Keel: assets/Keel.png and assets/Keel.icns.

Uses the "Heading" mark from the Keel brand assets (primary Sand tile or alt Dark tile)
and renders the standard macOS app icon: an 824x824 squircle tile with corner radius 185,
subtle drop shadow, centered on a 1024x1024 canvas, downsampled to all iconset resolutions.

Usage:
    python3 scripts/logo.py              # Builds assets/Keel.png & assets/Keel.icns
    python3 scripts/logo.py --alt        # Builds assets/Keel-alt.png & assets/Keel-alt.icns
    python3 scripts/logo.py --all        # Builds both primary and alt versions
"""

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path
from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
BRAND = ASSETS / "brand"

SS = 4                   # supersample factor for smooth antialiased corners
S, T, R = 1024, 824, 185 # canvas size, macOS tile size, corner radius
OFF = (S - T) // 2       # centered tile offset (100 px)

SIZES = [
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


def make_mac_icon(src_img: Image.Image) -> Image.Image:
    """Mask a 1024x1024 full-bleed tile into an 824x824 macOS squircle with drop shadow."""
    tile = src_img.convert("RGBA").resize((T, T), Image.LANCZOS)

    # Supersampled rounded-rectangle mask
    m = Image.new("L", (T * SS, T * SS), 0)
    ImageDraw.Draw(m).rounded_rectangle([0, 0, T * SS - 1, T * SS - 1], R * SS, fill=255)
    m = m.resize((T, T), Image.LANCZOS)
    tile.putalpha(m)

    # macOS bottom shadow
    sh = Image.new("L", (S, S), 0)
    sh.paste(m, (OFF, OFF + 10))
    sh = sh.filter(ImageFilter.GaussianBlur(14)).point(lambda v: int(v * 0.3))
    shadow = Image.new("RGBA", (S, S), (0, 0, 0, 255))
    shadow.putalpha(sh)

    out = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    out.alpha_composite(shadow)
    out.alpha_composite(tile, (OFF, OFF))
    return out


def make_icns(png_path: Path, icns_path: Path) -> None:
    """Generate a multi-resolution macOS .icns file from a master 1024x1024 PNG."""
    if shutil.which("iconutil") is None:
        print("iconutil not found; skipping .icns generation", file=sys.stderr)
        return

    iconset = icns_path.with_suffix(".iconset")
    iconset.mkdir(parents=True, exist_ok=True)
    try:
        base = Image.open(png_path)
        for fname, sz in SIZES:
            resized = base.resize((sz, sz), Image.LANCZOS)
            resized.save(iconset / fname)

        subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(icns_path)], check=True)
    finally:
        shutil.rmtree(iconset, ignore_errors=True)


def build_variant(name: str, src_png: Path) -> None:
    out_png = ASSETS / f"{name}.png"
    out_icns = ASSETS / f"{name}.icns"

    if not src_png.exists():
        raise FileNotFoundError(f"Source image not found: {src_png}")

    src = Image.open(src_png)
    icon = make_mac_icon(src)
    icon.save(out_png)
    print(f"Generated {out_png.relative_to(ROOT)} ({out_png.stat().st_size} bytes)")

    make_icns(out_png, out_icns)
    if out_icns.exists():
        print(f"Generated {out_icns.relative_to(ROOT)} ({out_icns.stat().st_size} bytes)")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--alt", action="store_true", help="Build dark alt variant only (Keel-alt)")
    parser.add_argument("--all", action="store_true", help="Build both primary and dark alt variants")
    args = parser.parse_args()

    primary_src = BRAND / "png" / "app-icon-1024.png"
    alt_src = BRAND / "png" / "app-icon-alt-1024.png"

    if args.all:
        build_variant("Keel", primary_src)
        build_variant("Keel-alt", alt_src)
    elif args.alt:
        build_variant("Keel-alt", alt_src)
    else:
        build_variant("Keel", primary_src)


if __name__ == "__main__":
    main()
