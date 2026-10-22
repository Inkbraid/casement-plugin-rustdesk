"""Draws icon.png: a monitor with a two-way relay arrow, on a transparent
background. Own artwork; run from the repository root."""
from PIL import Image, ImageDraw

S = 4  # drawn at 4x and scaled down for smooth edges
img = Image.new("RGBA", (256 * S, 256 * S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)
blue, dark, white = (38, 110, 196, 255), (22, 58, 110, 255), (255, 255, 255, 255)

def box(x0, y0, x1, y1):
    return [v * S for v in (x0, y0, x1, y1)]

d.rounded_rectangle(box(16, 32, 240, 186), radius=18 * S, fill=dark)       # monitor
d.rounded_rectangle(box(30, 46, 226, 172), radius=8 * S, fill=blue)        # screen
d.polygon([(v[0] * S, v[1] * S) for v in [(104, 186), (152, 186), (162, 218), (94, 218)]], fill=dark)
d.rounded_rectangle(box(72, 214, 184, 230), radius=6 * S, fill=dark)       # stand
# A two-way arrow across the screen: the relay.
d.rectangle(box(70, 94, 186, 104), fill=white)
d.polygon([(v[0] * S, v[1] * S) for v in [(186, 80), (212, 99), (186, 118)]], fill=white)
d.rectangle(box(70, 120, 186, 130), fill=white)
d.polygon([(v[0] * S, v[1] * S) for v in [(70, 106), (44, 125), (70, 144)]], fill=white)

img.resize((256, 256), Image.LANCZOS).save("icon.png")
