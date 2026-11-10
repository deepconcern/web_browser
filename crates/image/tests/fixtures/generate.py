"""Regenerates the tiny image fixtures in this directory.

Run with: python3 -I generate.py <this directory>   (needs Pillow)

PNG, GIF and lossless WebP fixtures are built inside the tests with the
`png`, `gif` and `image-webp` encoders instead; these files cover encoders
we don't link (JPEG, lossy and animated WebP).
"""

import sys
from pathlib import Path

from PIL import Image

out = Path(sys.argv[1])

RED, GREEN, BLUE, WHITE = (255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 255)


def quadrants(mode="RGB"):
    """32x16: red, green on top; blue, white below. 16x8 blocks keep JPEG
    blocks uniform so the colors survive compression."""
    img = Image.new("RGB", (32, 16))
    for (x0, y0, color) in [(0, 0, RED), (16, 0, GREEN), (0, 8, BLUE), (16, 8, WHITE)]:
        img.paste(color, (x0, y0, x0 + 16, y0 + 8))
    return img.convert(mode)


quadrants().save(out / "quadrants.jpg", quality=95, subsampling=0)
quadrants().save(out / "quadrants-progressive.jpg", quality=95, subsampling=0, progressive=True)
quadrants().save(out / "quadrants-420.jpg", quality=95, subsampling=2)
Image.new("L", (8, 8), 128).save(out / "gray.jpg", quality=95)
quadrants("CMYK").save(out / "quadrants-cmyk.jpg", quality=95)

quadrants().save(out / "quadrants-lossy.webp", quality=90)
alpha = Image.new("RGBA", (16, 16), (255, 0, 0, 255))
alpha.paste((0, 0, 255, 0), (0, 8, 16, 16))
alpha.save(out / "alpha-lossy.webp", quality=90)
frames = [Image.new("RGBA", (16, 16), (255, 0, 0, 255)), Image.new("RGBA", (16, 16), (0, 0, 255, 255))]
frames[0].save(out / "animated.webp", save_all=True, append_images=frames[1:], duration=100, lossless=True)
