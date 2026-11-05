"""Render the tray icon set from the Claude Design geometry (US00001).

Source: deliverable 1 of `dispatch/wiki/06_UX/Dispatch Mesh desktop app.html`
(16 px master: three-node mark plus a corner badge whose shape carries the
state). Run from the repository root with Pillow installed:

    python scripts/tray_icons.py

Writes `tray/icons/tray/<state>-<theme>-<px>.png` and `tray/icons/icon.ico`.
The design asks for hand hinting at 16 px before shipping; until a hinted set
replaces these files, they are supersampled renders of the master.
"""

from pathlib import Path

from PIL import Image, ImageDraw

SIZES = (16, 20, 24, 32)
SUPERSAMPLE = 16
MARK = {"light": (0x1C, 0x1D, 0x1F), "dark": (0xF2, 0xF2, 0xF2)}
# Badge tone per state and taskbar theme; the glyph is drawn in GLYPH on it.
TONE = {
    "working": {"light": (0x24, 0x57, 0xC5), "dark": (0x7F, 0xA6, 0xFF)},
    "offline": {"light": (0x5B, 0x61, 0x70), "dark": (0xA7, 0xAD, 0xB8)},
    "attention": {"light": (0xC2, 0x54, 0x0F), "dark": (0xFF, 0x9E, 0x57)},
}
GLYPH = {"light": (0xFF, 0xFF, 0xFF), "dark": (0x14, 0x16, 0x1A)}
STATES = ("synced", "working", "offline", "attention")


def render(state: str, theme: str, px: int) -> Image.Image:
    k = px * SUPERSAMPLE / 16  # design units -> supersampled pixels

    def p(x: float, y: float) -> tuple[float, float]:
        return (x * k, y * k)

    def disc(draw: ImageDraw.ImageDraw, cx: float, cy: float, r: float, fill) -> None:
        draw.ellipse((*p(cx - r, cy - r), *p(cx + r, cy + r)), fill=fill)

    size = round(16 * k)
    mark = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(mark)
    alpha = 128 if state == "offline" else 255
    color = (*MARK[theme], alpha)
    width = round(1.4 * k)
    for a, b in (((4, 4), (12, 4)), ((4, 4), (4, 12)), ((12, 4), (4, 12))):
        draw.line((*p(*a), *p(*b)), fill=color, width=width)
    for cx, cy in ((4, 4), (12, 4), (4, 12)):
        disc(draw, cx, cy, 2.2, color)

    if state != "synced":
        # Knock the mark out around the badge so it reads on any taskbar.
        knockout = Image.new("L", (size, size), 255)
        disc(ImageDraw.Draw(knockout), 12, 12, 4.6, 0)
        mark.putalpha(Image.composite(mark.getchannel("A"), knockout, knockout))
        draw = ImageDraw.Draw(mark)
        disc(draw, 12, 12, 3.6, (*TONE[state][theme], 255))
        glyph = (*GLYPH[theme], 255)
        stroke = round(1.3 * k)
        cap = 1.3 * k / 2
        if state == "working":
            # "M12 10a2 2 0 1 1-2 2": three quarters of a circle, top to left.
            draw.arc((*p(10, 10), *p(14, 14)), start=-90, end=180, fill=glyph, width=stroke)
            for x, y in ((12, 10), (10, 12)):
                draw.ellipse((x * k - cap, y * k - cap, x * k + cap, y * k + cap), fill=glyph)
        elif state == "offline":
            draw.line((*p(10.5, 13.5), *p(13.5, 10.5)), fill=glyph, width=stroke)
            for x, y in ((10.5, 13.5), (13.5, 10.5)):
                draw.ellipse((x * k - cap, y * k - cap, x * k + cap, y * k + cap), fill=glyph)
        else:  # attention: "M12 10v2.3M12 14.1v0.05"
            draw.line((*p(12, 10), *p(12, 12.3)), fill=glyph, width=stroke)
            for x, y in ((12, 10), (12, 12.3), (12, 14.1)):
                draw.ellipse((x * k - cap, y * k - cap, x * k + cap, y * k + cap), fill=glyph)
    return mark.resize((px, px), Image.LANCZOS)


def main() -> None:
    root = Path(__file__).resolve().parent.parent / "tray" / "icons"
    out = root / "tray"
    out.mkdir(parents=True, exist_ok=True)
    for state in STATES:
        for theme in MARK:
            for px in SIZES:
                render(state, theme, px).save(out / f"{state}-{theme}-{px}.png")
    # Application icon: the plain mark in the working tone, for window and file.
    mark = render("synced", "light", 256)
    tinted = Image.new("RGBA", mark.size, (*TONE["working"]["light"], 255))
    tinted.putalpha(mark.getchannel("A"))
    tinted.save(root / "icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (256, 256)])


if __name__ == "__main__":
    main()
