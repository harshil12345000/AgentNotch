"""Builds the README / website screenshots from Notch's own off-screen renders (demo data).
Run from the repo root after `cargo build --release`:  python assets/make_screens.py
Needs Pillow."""
import os, subprocess, tempfile
from PIL import Image, ImageDraw, ImageFilter

EXE = os.path.join("target", "release", "notch.exe")
OUT = os.path.join("docs", "screenshots")
BG = (112, 128, 144)  # the grey Notch composites its snapshots over
os.makedirs(OUT, exist_ok=True)
tmp = tempfile.mkdtemp()

def render(name, *args):
    path = os.path.join(tmp, name + ".bmp")
    subprocess.run([EXE, "--snapshot", path, *args], check=True)
    return Image.open(path).convert("RGB")

def crop(img, pad=0):
    """Cut the render down to the notch itself."""
    px = img.load()
    w, h = img.size
    xs = [x for x in range(w) for y in range(0, h, 2) if px[x, y] != BG]
    ys = [y for y in range(h) for x in range(0, w, 2) if px[x, y] != BG]
    return img.crop((max(min(xs) - pad, 0), max(min(ys) - pad, 0), min(max(xs) + 1 + pad, w), min(max(ys) + 1 + pad, h)))

def backdrop(w, h):
    """A soft desktop-like gradient to set the notch on."""
    img = Image.new("RGB", (w, h))
    d = ImageDraw.Draw(img)
    top, bottom = (96, 112, 134), (142, 150, 170)
    for y in range(h):
        t = y / max(h - 1, 1)
        d.line([(0, y), (w, y)], fill=tuple(int(a + (b - a) * t) for a, b in zip(top, bottom)))
    return img

def place(canvas, shot, x, y):
    """Paste a crop, treating the snapshot grey as transparent so edges blend."""
    mask = Image.new("L", shot.size, 0)
    sp, mp = shot.load(), mask.load()
    for yy in range(shot.size[1]):
        for xx in range(shot.size[0]):
            r, g, b = sp[xx, yy]
            # distance from the snapshot grey -> opacity
            d = abs(r - BG[0]) + abs(g - BG[1]) + abs(b - BG[2])
            mp[xx, yy] = min(255, d * 3)
    canvas.paste(shot, (x, y), mask)

shots = {
    "collapsed": crop(render("collapsed", "--edge", "top")),
    "expanded": crop(render("expanded", "--edge", "top", "--expanded")),
    "side-tab": crop(render("sidetab", "--edge", "right")),
    "side": crop(render("side", "--edge", "right", "--expanded")),
    "settings": crop(render("settings", "--edge", "top", "--settings")),
}

# individual screenshots, each on a backdrop, docked the way it sits on screen
def single(name, shot, dock):
    pad = 70
    w, h = shot.size[0] + pad * 2, shot.size[1] + pad * 2
    if dock == "top":
        c = backdrop(w, shot.size[1] + pad)
        place(c, shot, pad, 0)
    else:  # right edge
        c = backdrop(shot.size[0] + pad, h)
        place(c, shot, pad, pad)
    c.save(os.path.join(OUT, name + ".png"), optimize=True)
    print(name, c.size)

single("collapsed", shots["collapsed"], "top")
single("expanded", shots["expanded"], "top")
single("side-tab", shots["side-tab"], "right")
single("side", shots["side"], "right")
single("settings", shots["settings"], "top")

# hero: a wide "desktop" with the open panel at the top centre and the side tab on the right
W, H = 1600, 900
hero = backdrop(W, H)
e, s = shots["expanded"], shots["side-tab"]
place(hero, e, (W - e.size[0]) // 2, 0)
place(hero, s, W - s.size[0], 520)
hero.save(os.path.join(OUT, "hero.png"), optimize=True)
print("hero", hero.size)
