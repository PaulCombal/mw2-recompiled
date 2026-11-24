#!/usr/bin/env python3
"""Make the game's and the launcher's icons from the campaign's icon.

    tools/icons.py <campaign icon>

The campaign icon is the 256-pixel one in the PC game's iw4sp.exe:
    7z x iw4sp.exe .rsrc/ICON/5.ico
Writes into assets/icons/, each an .ico (what Windows shows for the program,
by the .rc beside it) and a .bmp (what the window is given, on both systems):
    sp        the campaign's icon without its 2
    mp        the same in the multiplayer's colours
    launcher  the campaign's icon with its 2 on the left, the same in the
              multiplayer's colours on the right

The multiplayer's colours are a fit on the game's two 32-pixel Steam icons,
which are one picture in the two colours: red takes the green, green a mix of
the red and the green, blue is dimmed.

The 2 comes out in three steps, each fitted to this one picture:
  - its fill: the numeral's light green, in pieces of more than 400 pixels (the
    waveform's are smaller, but for one on the right, past the numeral, and one
    on the left, whose part left of the numeral's diagonal is dropped);
  - its outline and glow: grown from the fill through every pixel brighter
    than the dark background, at most 16 pixels, then 2 more all round;
  - filled with the background as the dark pixels around give it, smoothed in,
    and the waveform the numeral hid drawn again through it: the baseline, and
    the pulse whose top shows between the numeral's two halves.
"""
import os, sys
from PIL import Image, ImageChops, ImageDraw, ImageFilter

MULTIPLAYER = (0, 1, 0, 0,  .62, .30, 0, 0,  0, 0, .70, 0)
SIZES = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]

FILL = {(204, 255, 153), (204, 255, 204), (204, 255, 102)}
DARK = 25  # the background's brightness, at most
WAVE = [(40, 128.3), (80, 128.3), (112, 128.3), (117, 124), (121, 112), (125, 98), (129, 86.5), (132, 90),
        (135, 101), (138, 113), (141, 126), (145, 139), (150, 147.5), (154, 145), (157, 136), (160, 128.8),
        (165, 128.3), (200, 128.3), (225, 128.3)]


def brightness(pixel):
    return (pixel[0] * 3 + pixel[1] * 6 + pixel[2]) // 10


def numeral(picture):
    """The pixels the 2 covers, its outline and glow included."""
    width, height = picture.size
    pixels = picture.load()
    near = lambda x, y: [(x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)]
    inside = lambda p: 0 <= p[0] < width and 0 <= p[1] < height
    seen, fill = set(), []
    for y in range(height):
        for x in range(width):
            if (x, y) in seen or pixels[x, y] not in FILL:
                continue
            seen.add((x, y))
            piece, stack = [], [(x, y)]
            while stack:
                point = stack.pop()
                piece.append(point)
                for next in near(*point):
                    if inside(next) and next not in seen and pixels[next] in FILL:
                        seen.add(next)
                        stack.append(next)
            if len(piece) > 400 and max(p[0] for p in piece) <= 215:
                fill += [p for p in piece if p[0] >= 88 or p[1] >= 134 or p[1] < 100]
    distance = {point: 0 for point in fill}
    front = fill
    while front:
        grown = []
        for point in front:
            for next in near(*point):
                if inside(next) and next not in distance and distance[point] < 16 and brightness(pixels[next]) > DARK:
                    distance[next] = distance[point] + 1
                    grown.append(next)
        front = grown
    mask = Image.new("L", picture.size, 0)
    for point in distance:
        mask.putpixel(point, 255)
    return mask.filter(ImageFilter.MaxFilter(3)).filter(ImageFilter.MinFilter(3)).filter(ImageFilter.MaxFilter(5))


def without_numeral(campaign):
    picture = campaign.convert("RGB")
    width, height = picture.size
    mask = numeral(picture)
    # The background: what the dark pixels give, spread over everything else.
    unknown = Image.new("L", picture.size, 0)
    pixels = picture.load()
    for y in range(height):
        for x in range(width):
            if brightness(pixels[x, y]) > DARK or mask.getpixel((x, y)):
                unknown.putpixel((x, y), 255)
    background = Image.composite(Image.new("RGBA", picture.size, (17, 17, 17, 255)), campaign, unknown)
    for _ in range(400):
        background = Image.composite(background.filter(ImageFilter.BoxBlur(1)), campaign, unknown)
    icon = Image.composite(background, campaign, mask.filter(ImageFilter.GaussianBlur(0.7)))
    # The waveform, drawn at four times the size, and kept to where the numeral was.
    def spline(p0, p1, p2, p3, t):
        return tuple(.5 * (2 * p1[i] + (p2[i] - p0[i]) * t + (2 * p0[i] - 5 * p1[i] + 4 * p2[i] - p3[i]) * t * t
                           + (3 * p1[i] - p0[i] - 3 * p2[i] + p3[i]) * t ** 3) for i in (0, 1))
    points = [WAVE[0]] + WAVE + [WAVE[-1]]
    line = [spline(*points[i - 1:i + 3], k / 20) for i in range(1, len(points) - 2) for k in range(20)] + [WAVE[-1]]
    where = mask.filter(ImageFilter.MaxFilter(3)).filter(ImageFilter.GaussianBlur(1))
    for thickness, colour, blur in ((3.5, (51, 153, 0, 150), 1.5), (1.1, (204, 255, 153, 255), 0)):
        layer = Image.new("RGBA", (width * 4, height * 4), (0, 0, 0, 0))
        ImageDraw.Draw(layer).line([(x * 4, y * 4) for x, y in line], fill=colour, width=int(thickness * 4), joint="curve")
        layer = layer.resize(picture.size, Image.LANCZOS)
        if blur:
            layer = layer.filter(ImageFilter.GaussianBlur(blur))
        layer.putalpha(ImageChops.multiply(layer.getchannel("A"), where))
        icon.alpha_composite(layer)
    icon.putalpha(campaign.getchannel("A"))
    return icon


def multiplayer(picture):
    coloured = picture.convert("RGB").convert("RGB", MULTIPLAYER).convert("RGBA")
    coloured.putalpha(picture.getchannel("A"))
    return coloured


def save(icon, path):
    icon.save(path + ".ico", sizes=SIZES)
    icon.save(path + ".bmp")
    print("wrote", os.path.relpath(path, root) + ".ico and .bmp")


if len(sys.argv) != 2:
    raise SystemExit(__doc__)
campaign = Image.open(sys.argv[1]).convert("RGBA")
if campaign.size != (256, 256):
    raise SystemExit(f"{sys.argv[1]}: {campaign.size[0]}x{campaign.size[1]}, the 256-pixel icon is wanted")
root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")

launcher = campaign.copy()
launcher.paste(multiplayer(campaign).crop((128, 0, 256, 256)), (128, 0))
icons = os.path.join(root, "assets", "icons")
save(launcher, os.path.join(icons, "launcher"))

game = without_numeral(campaign)
save(game, os.path.join(icons, "sp"))
save(multiplayer(game), os.path.join(icons, "mp"))
