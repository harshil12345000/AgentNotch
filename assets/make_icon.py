"""Draws assets/notch.ico: a dark rounded tile with a notch pill and three status dots.
Run:  python assets/make_icon.py   (needs Pillow)"""
from PIL import Image, ImageDraw
import os

S = 1024  # draw large, then downsample for clean edges
img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)
# tile
d.rounded_rectangle((40, 40, S - 40, S - 40), radius=230, fill=(17, 17, 21, 255))
d.rounded_rectangle((40, 40, S - 40, S - 40), radius=230, outline=(255, 255, 255, 34), width=10)
# the notch pill
d.rounded_rectangle((170, 392, S - 170, 632), radius=120, fill=(0, 0, 0, 255))
d.rounded_rectangle((170, 392, S - 170, 632), radius=120, outline=(255, 255, 255, 46), width=8)
# three status dots: working, needs input, finished
for cx, col in ((332, (59, 155, 255)), (512, (255, 176, 32)), (692, (52, 199, 89))):
    d.ellipse((cx - 62, 512 - 62, cx + 62, 512 + 62), fill=col + (255,))

out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "notch.ico")
sizes = [16, 20, 24, 32, 40, 48, 64, 256]
img.resize((256, 256), Image.LANCZOS).save(out, format="ICO", sizes=[(s, s) for s in sizes])
img.resize((256, 256), Image.LANCZOS).save(os.path.join(os.path.dirname(out), "notch.png"))
print("wrote", out, os.path.getsize(out), "bytes")
