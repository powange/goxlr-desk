#!/usr/bin/env python3
"""Draws the GoXLR Desk icon (four faders on a dark tile) and writes every file the build uses.

The logo is only rounded rectangles, so it's drawn here directly rather than rasterised from
an SVG: each size gets the version made for it (thicker rails at 32 px, sliders only at 16 px).
Needs Pillow. Run from anywhere: the files land next to this script.

    goxlr-desk.svg          scalable icon (Linux, interface)
    goxlr-desk.png          48 px (Linux menus)
    goxlr-desk-large.png    128 px (Linux pixmaps, tray)
    goxlr-desk.ico          16 to 256 px (Windows executables and installer)
    icon.icns               16 to 1024 px (macOS)

Options: `--favicon PATH` also writes the web interface's favicon.ico there; `--tauri DIR` also
writes the icons the desktop app's build takes (see ci/rebrand-ui-wrapper.sh).
"""
import sys
from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent

TILE = "#0d1416"
RAIL = "#34474b"
SLIDER = "#4cc3c6"

RAILS_X = [17, 27, 37, 47]
# Each slider's top, in the 64 unit grid
SLIDERS_Y = [31, 19, 37, 25]


def design(size):
    """The drawing for a size: rail width and slider box, in 64 units (None: no rails)."""
    if size <= 16:
        return None, (12, 9), -2
    if size <= 32:
        return 4, (12, 8), -1
    if size <= 48:
        return 3.5, (10, 7), 0
    return 3, (10, 6), 0


def draw(size):
    scale = 8
    big = size * scale
    unit = big / 64
    image = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    pen = ImageDraw.Draw(image)
    pen.rounded_rectangle([2 * unit, 2 * unit, 62 * unit, 62 * unit], radius=14 * unit, fill=TILE)

    rail, (width, height), shift = design(size)
    if rail is not None:
        for x in RAILS_X:
            pen.rounded_rectangle(
                [(x - rail / 2) * unit, (14 - rail / 2) * unit, (x + rail / 2) * unit, (50 + rail / 2) * unit],
                radius=rail / 2 * unit,
                fill=RAIL,
            )
    for x, y in zip(RAILS_X, SLIDERS_Y):
        top = y + shift
        pen.rounded_rectangle(
            [(x - width / 2) * unit, top * unit, (x + width / 2) * unit, (top + height) * unit],
            radius=2 * unit,
            fill=SLIDER,
        )
    return image.resize((size, size), Image.LANCZOS)


def svg():
    rails = "".join(f'<line x1="{x}" y1="14" x2="{x}" y2="50"/>' for x in RAILS_X)
    sliders = "".join(
        f'<rect x="{x - 5}" y="{y}" width="10" height="6" rx="2"/>' for x, y in zip(RAILS_X, SLIDERS_Y)
    )
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">'
        f'<rect x="2" y="2" width="60" height="60" rx="14" fill="{TILE}"/>'
        f'<g stroke="{RAIL}" stroke-width="3" stroke-linecap="round">{rails}</g>'
        f'<g fill="{SLIDER}">{sliders}</g></svg>\n'
    )


def write_ico(path, sizes):
    images = [draw(size) for size in sizes]
    # PNG-compressed entries: with the same icon as uncompressed bitmaps, Defender's machine
    # learning flagged the launcher (Trojan:Win32/Bearfoos.A!ml, a false positive)
    images[-1].save(path, format="ICO", sizes=[(s, s) for s in sizes], append_images=images[:-1])


def write_icns(path):
    sizes = [16, 32, 64, 128, 256, 512, 1024]
    images = [draw(size) for size in sizes]
    images[-1].save(path, format="ICNS", append_images=images[:-1])


def main():
    (HERE / "goxlr-desk.svg").write_text(svg())
    draw(48).save(HERE / "goxlr-desk.png")
    draw(128).save(HERE / "goxlr-desk-large.png")
    write_ico(HERE / "goxlr-desk.ico", [16, 32, 48, 128, 256])
    write_icns(HERE / "icon.icns")

    args = sys.argv[1:]
    if "--favicon" in args:
        write_ico(Path(args[args.index("--favicon") + 1]), [16, 32, 48])
    if "--tauri" in args:
        tauri = Path(args[args.index("--tauri") + 1])
        tauri.mkdir(parents=True, exist_ok=True)
        write_ico(tauri / "icon.ico", [16, 24, 32, 48, 64, 128, 256])
        write_icns(tauri / "icon.icns")
        for name, size in [("32x32.png", 32), ("128x128.png", 128), ("128x128@2x.png", 256), ("icon.png", 512)]:
            draw(size).save(tauri / name)


if __name__ == "__main__":
    main()
