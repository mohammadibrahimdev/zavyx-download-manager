"""Generate the Zavyx DM (Zavyx Download Manager) logo in every size the project needs.

Requires Pillow (`pip install pillow`) and Windows fonts (Segoe UI). Run from the
repo root:

    python scripts/gen_icons.py

Two artworks share the Zavyx "Z" mark:
    full  - Z with "DOWNLOAD / MANAGER" underneath (sizes where text is readable)
    small - Z with a download arrow underneath (16-64 px, toolbar/taskbar sizes)

Outputs:
    assets/logo.svg              full logo, vector (README, website)
    assets/logo-256.png          full logo (app window, About dialog)
    assets/zavyx-dm.ico          Windows exe / installer icon (small + full sizes)
    extension/icons/iconN.png    browser extension icons (16/32/48/128)
"""
import os

from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FONT = r"C:\Windows\Fonts\seguibl.ttf"  # Segoe UI Black

# Brand colours: "Zavyx violet" gradient.
TOP = (139, 108, 255)
BOTTOM = (91, 52, 230)

# The "Z" mark on a 256-unit canvas.
_Z = [(64, 64), (192, 64), (192, 98), (110, 160), (192, 160),
      (192, 194), (64, 194), (64, 160), (146, 98), (64, 98)]


def z_points(x0, y0, w, h):
    return [(x0 + (x - 64) * w / 128, y0 + (y - 64) * h / 130) for x, y in _Z]


Z_FULL = z_points(83, 34, 90, 92)    # leaves room for two lines of text
Z_SMALL = z_points(68, 30, 120, 122)  # leaves room for the arrow
# Download arrow under the small mark: shaft + head, then a tray line.
ARROW = [(116, 160), (140, 160), (140, 186), (160, 186), (128, 216), (96, 186), (116, 186)]
TRAY = (84, 222, 172, 236)


def tile(big):
    """Rounded violet gradient square of `big` pixels; returns it and the scale."""
    k = big / 256
    gradient = Image.new("RGBA", (big, big))
    draw = ImageDraw.Draw(gradient)
    for y in range(big):
        t = y / (big - 1)
        colour = tuple(round(a + (b - a) * t) for a, b in zip(TOP, BOTTOM))
        draw.line([(0, y), (big, y)], fill=colour + (255,))
    mask = Image.new("L", (big, big), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, big - 1, big - 1], radius=int(56 * k), fill=255)
    img = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    img.paste(gradient, (0, 0), mask)
    return img, k


def render(size, full):
    scale = 4  # supersample, then downscale for smooth edges
    img, k = tile(size * scale)
    draw = ImageDraw.Draw(img)
    white = (255, 255, 255, 255)
    if full:
        draw.polygon([(x * k, y * k) for x, y in Z_FULL], fill=white)
        font = ImageFont.truetype(FONT, int(31 * k))
        for text, y in (("DOWNLOAD", 146), ("MANAGER", 184)):
            w = draw.textlength(text, font=font)
            draw.text(((256 * k - w) / 2, y * k), text, font=font, fill=white)
    else:
        draw.polygon([(x * k, y * k) for x, y in Z_SMALL], fill=white)
        draw.polygon([(x * k, y * k) for x, y in ARROW], fill=white)
        draw.rounded_rectangle([v * k for v in TRAY], radius=int(6 * k), fill=white)
    return img.resize((size, size), Image.LANCZOS)


SVG = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" width="256" height="256">
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#{top}"/>
      <stop offset="1" stop-color="#{bottom}"/>
    </linearGradient>
  </defs>
  <rect width="256" height="256" rx="56" fill="url(#g)"/>
  <polygon fill="#fff" points="{points}"/>
  <g fill="#fff" font-family="'Segoe UI Black','Arial Black',sans-serif" font-weight="900" font-size="31" text-anchor="middle">
    <text x="128" y="177">DOWNLOAD</text>
    <text x="128" y="215">MANAGER</text>
  </g>
</svg>
"""


def main():
    assets = os.path.join(ROOT, "assets")
    icons = os.path.join(ROOT, "extension", "icons")
    os.makedirs(assets, exist_ok=True)
    os.makedirs(icons, exist_ok=True)

    with open(os.path.join(assets, "logo.svg"), "w", encoding="utf-8") as f:
        f.write(SVG.format(
            top="%02x%02x%02x" % TOP,
            bottom="%02x%02x%02x" % BOTTOM,
            points=" ".join(f"{x:.1f},{y:.1f}" for x, y in Z_FULL),
        ))

    render(256, full=True).save(os.path.join(assets, "logo-256.png"))

    # ICO: small artwork for 16-64 px, full artwork for 128/256 px.
    small_sizes, full_sizes = [16, 24, 32, 48, 64], [128, 256]
    frames = [render(s, full=False) for s in small_sizes] + [render(s, full=True) for s in full_sizes]
    frames[-1].save(
        os.path.join(assets, "zavyx-dm.ico"),
        sizes=[(s, s) for s in small_sizes + full_sizes],
        append_images=frames[:-1],
    )

    for s in (16, 32, 48):
        render(s, full=False).save(os.path.join(icons, f"icon{s}.png"))
    render(128, full=True).save(os.path.join(icons, "icon128.png"))
    print("icons written")


if __name__ == "__main__":
    main()
