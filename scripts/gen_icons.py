"""Generate the Zavyx logo in every size the project needs.

Requires Pillow (`pip install pillow`). Run from the repo root:

    python scripts/gen_icons.py

Outputs:
    assets/logo.svg              vector logo (README, website)
    assets/logo-256.png          app window icon
    assets/zavyx.ico             Windows exe / installer icon
    extension/icons/iconN.png    browser extension icons (16/32/48/128)
"""
import os

from PIL import Image, ImageDraw

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Brand colours: "Zavyx violet" gradient.
TOP = (139, 108, 255)
BOTTOM = (91, 52, 230)

# The "Z" mark on a 256-unit canvas.
Z_POINTS = [(64, 64), (192, 64), (192, 98), (110, 160), (192, 160),
            (192, 194), (64, 194), (64, 160), (146, 98), (64, 98)]


def render(size):
    scale = 4  # supersample, then downscale for smooth edges
    big = size * scale
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
    ImageDraw.Draw(img).polygon([(x * k, y * k) for x, y in Z_POINTS], fill=(255, 255, 255, 255))
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
            points=" ".join(f"{x},{y}" for x, y in Z_POINTS),
        ))

    render(256).save(os.path.join(assets, "logo-256.png"))
    sizes = [16, 24, 32, 48, 64, 128, 256]
    render(256).save(os.path.join(assets, "zavyx.ico"), sizes=[(s, s) for s in sizes])
    for s in (16, 32, 48, 128):
        render(s).save(os.path.join(icons, f"icon{s}.png"))
    print("icons written")


if __name__ == "__main__":
    main()
