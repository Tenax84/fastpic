"""Regenerate the FastPic application icon assets."""

from __future__ import annotations

import io
import struct
from pathlib import Path

from PIL import Image, ImageDraw


HERE = Path(__file__).resolve().parent
SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256)
SUPERSAMPLE = 4

SVG = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" role="img" aria-label="FastPic icon">
  <defs>
    <linearGradient id="tile" x1="34" y1="28" x2="224" y2="232" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#00A8FF"/>
      <stop offset="0.48" stop-color="#3478F6"/>
      <stop offset="1" stop-color="#8B38F5"/>
    </linearGradient>
  </defs>
  <rect x="16" y="16" width="224" height="224" rx="52" fill="url(#tile)"/>
  <circle cx="174" cy="79" r="18" fill="#fff"/>
  <path fill="#fff" fill-rule="evenodd" d="M45 178c0-4 1-7 4-10l48-58c4-5 11-5 15 0l29 34 14-17c4-5 11-5 15 0l40 48c6 7 1 17-8 17H55c-6 0-10-5-10-10v-4Zm25-116h35L90 87h20l-43 49 10-37H55l15-37Z"/>
</svg>
"""


def _gradient(size: int) -> Image.Image:
    """Blue-to-violet diagonal gradient at the requested render size."""
    image = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    pixels = image.load()
    stops = ((0.0, (0, 168, 255)), (0.48, (52, 120, 246)), (1.0, (139, 56, 245)))
    for y in range(size):
        for x in range(size):
            t = max(0.0, min(1.0, (x + y - 32 / 256 * size) / (384 / 256 * size)))
            for index in range(len(stops) - 1):
                p0, c0 = stops[index]
                p1, c1 = stops[index + 1]
                if t <= p1:
                    u = (t - p0) / (p1 - p0)
                    rgb = tuple(round(a + (b - a) * u) for a, b in zip(c0, c1))
                    pixels[x, y] = (*rgb, 255)
                    break
    return image


def render_icon(size: int) -> Image.Image:
    """Render one independently rasterized, supersampled icon frame."""
    scale = size * SUPERSAMPLE / 256
    canvas_size = size * SUPERSAMPLE

    def p(value: float) -> int:
        return round(value * scale)

    canvas = Image.new("RGBA", (canvas_size, canvas_size), (0, 0, 0, 0))
    tile_mask = Image.new("L", canvas.size, 0)
    ImageDraw.Draw(tile_mask).rounded_rectangle(
        (p(16), p(16), p(240), p(240)), radius=p(52), fill=255
    )
    canvas.alpha_composite(Image.composite(_gradient(canvas_size), canvas, tile_mask))

    draw = ImageDraw.Draw(canvas)
    white = (255, 255, 255, 255)
    draw.ellipse((p(156), p(61), p(192), p(97)), fill=white)

    # The landscape and bolt are deliberately solid silhouettes: no strokes vanish
    # at 16 px, and their negative-space separation remains visible.
    mountains = [
        (45, 178), (49, 168), (97, 110), (104, 106), (112, 110),
        (141, 144), (155, 127), (162, 123), (170, 127), (210, 175),
        (212, 182), (208, 189), (202, 192), (55, 192), (48, 189),
        (45, 182),
    ]
    draw.polygon([(p(x), p(y)) for x, y in mountains], fill=white)
    bolt = [(70, 62), (105, 62), (90, 87), (110, 87), (67, 136), (77, 99), (55, 99)]
    draw.polygon([(p(x), p(y)) for x, y in bolt], fill=white)

    return canvas.resize((size, size), Image.Resampling.LANCZOS)


def write_ico(frames: list[Image.Image], destination: Path) -> None:
    """Write PNG-compressed, 32-bit RGBA frames into a Windows ICO container."""
    payloads: list[bytes] = []
    for frame in frames:
        stream = io.BytesIO()
        frame.save(stream, format="PNG", optimize=True)
        payloads.append(stream.getvalue())

    header_size = 6 + 16 * len(frames)
    offset = header_size
    entries = []
    for frame, payload in zip(frames, payloads):
        width, height = frame.size
        entries.append(
            struct.pack(
                "<BBBBHHII",
                0 if width == 256 else width,
                0 if height == 256 else height,
                0,
                0,
                1,
                32,
                len(payload),
                offset,
            )
        )
        offset += len(payload)

    destination.write_bytes(
        struct.pack("<HHH", 0, 1, len(frames)) + b"".join(entries) + b"".join(payloads)
    )


def main() -> None:
    HERE.mkdir(parents=True, exist_ok=True)
    (HERE / "fastpic.svg").write_text(SVG, encoding="utf-8", newline="\n")
    frames = [render_icon(size) for size in SIZES]
    frames[-1].save(HERE / "fastpic.png", format="PNG", optimize=True)
    write_ico(frames, HERE / "fastpic.ico")
    print("ICO sizes:", ", ".join(f"{size}x{size}" for size in SIZES))


if __name__ == "__main__":
    main()
