#!/usr/bin/env python3
"""Generates the shared nuvia icon (window, tray, installer, packages)."""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = Path(__file__).resolve().parent.parent
SIZE = 256
FONT = ROOT / "crates/nuvia-gui/assets/Rajdhani-Bold.ttf"
GREEN = (0, 245, 160, 255)
GREEN_SOFT = (0, 245, 160, 180)
DARK_TOP = (12, 20, 31, 255)
DARK_BOTTOM = (5, 8, 13, 255)


def make_icon() -> Image.Image:
    icon = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    mask = Image.new("L", (SIZE, SIZE), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [7, 7, SIZE - 8, SIZE - 8], radius=52, fill=255
    )

    gradient = Image.new("RGBA", (SIZE, SIZE))
    draw = ImageDraw.Draw(gradient)
    for y in range(SIZE):
        t = y / (SIZE - 1)
        color = tuple(
            int(DARK_TOP[i] + (DARK_BOTTOM[i] - DARK_TOP[i]) * t) for i in range(3)
        ) + (255,)
        draw.line([(0, y), (SIZE, y)], fill=color)
    icon.paste(gradient, (0, 0), mask)

    border = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    ImageDraw.Draw(border).rounded_rectangle(
        [7, 7, SIZE - 8, SIZE - 8], radius=52, outline=GREEN, width=5
    )
    icon = Image.alpha_composite(icon, border)

    font = ImageFont.truetype(str(FONT), 202)
    text = "N"
    box = font.getbbox(text)
    width = box[2] - box[0]
    height = box[3] - box[1]
    position = ((SIZE - width) / 2 - box[0], (SIZE - height) / 2 - box[1])

    letter = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    ImageDraw.Draw(letter).text(position, text, font=font, fill=GREEN)

    glow = letter.filter(ImageFilter.GaussianBlur(8))
    glow_color = Image.new("RGBA", (SIZE, SIZE), (0, 245, 160, 0))
    glow_color.putalpha(glow.getchannel("A").point(lambda a: int(a * 0.5)))
    icon = Image.alpha_composite(icon, glow_color)

    soft = letter.filter(ImageFilter.GaussianBlur(2))
    icon = Image.alpha_composite(icon, soft)
    icon = Image.alpha_composite(icon, letter)
    return icon


def main() -> None:
    icon = make_icon()
    assets = ROOT / "crates/nuvia-gui/assets"
    linux = ROOT / "deploy/linux"
    windows = ROOT / "deploy/windows"
    linux.mkdir(parents=True, exist_ok=True)

    (assets / "nuvia-icon.rgba").write_bytes(icon.tobytes())
    icon.save(linux / "nuvia-256.png")
    icon.resize((128, 128), Image.LANCZOS).save(linux / "nuvia-128.png")
    icon.resize((64, 64), Image.LANCZOS).save(linux / "nuvia-64.png")
    icon.save(
        windows / "nuvia.ico",
        format="ICO",
        sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    icon.save(
        windows / "nuvia-nsis.ico",
        format="ICO",
        sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128)],
        bitmap_format="bmp",
    )
    print("wrote nuvia-icon.rgba, PNGs and ICOs")


if __name__ == "__main__":
    main()
