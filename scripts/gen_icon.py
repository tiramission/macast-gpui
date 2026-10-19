"""Generate the app icon (blue rounded square + white play mark).

    python scripts/gen_icon.py

Writes crates/macast-app/assets/icon.ico (16-256 px) and icon.png.
The .ico is embedded into the exe by crates/macast-app/build.rs as resource
id 1, which is the id gpui's Windows backend loads for the window class.
"""

from pathlib import Path

from PIL import Image, ImageDraw

SIZE = 1024
BLUE = (47, 111, 235, 255)
WHITE = (255, 255, 255, 255)

img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
draw = ImageDraw.Draw(img)

# Rounded square with a little breathing room, like a modern app tile.
margin = int(SIZE * 0.06)
draw.rounded_rectangle(
    [margin, margin, SIZE - margin, SIZE - margin],
    radius=int(SIZE * 0.22),
    fill=BLUE,
)

# Play triangle, optically centred (shifted right so it does not look left-heavy).
left = int(SIZE * 0.38)
right = int(SIZE * 0.71)
top = int(SIZE * 0.31)
bottom = int(SIZE * 0.69)
draw.polygon([(left, top), (left, bottom), (right, SIZE // 2)], fill=WHITE)

out_dir = Path(__file__).resolve().parent.parent / "crates" / "macast-app" / "assets"
out_dir.mkdir(parents=True, exist_ok=True)

icon = img.resize((256, 256), Image.LANCZOS)
icon.save(out_dir / "icon.png")
icon.save(
    out_dir / "icon.ico",
    sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
)
print(f"wrote {out_dir / 'icon.ico'} and icon.png")
