#!/usr/bin/env python3
"""Make the README's pictures in docs/images/readme/: the banner, the download
button, the feature tiles and the section headings.

    tools/readme_art.py

Needs fontTools (pip install fonttools). Every letter is drawn as a path, so
that the pictures look the same wherever GitHub shows them, with no font to
load. The style is the game's title screen and the launcher's: grey smoke lit
from the upper right, an amber glow in the bottom left, condensed capitals
(Barlow, the launcher's fonts) and the title in Michroma, all from
third_party/fonts/. The smoke is SVG noise with fixed seeds, so the output is
the same on every run.
"""
import os
from fontTools.ttLib import TTFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FONTS = os.path.join(REPO, 'third_party/fonts')
OUT = os.path.join(REPO, 'docs/images/readme')

WHITE = '#ECECE8'
AMBER = '#E8B234'
GREEN = '#BFF2B0'

_fonts = {}
def font(name):
    if name not in _fonts:
        f = TTFont(os.path.join(FONTS, name))
        _fonts[name] = (f, f.getGlyphSet(), f.getBestCmap(), f['hmtx'], f['head'].unitsPerEm)
    return _fonts[name]

MENU = 'BarlowCondensed-Regular.ttf'
HEAD = 'BarlowCondensed-Medium.ttf'
BODY = 'BarlowSemiCondensed-Regular.ttf'
TITLE = 'Michroma-Regular.ttf'

def measure(name, text, size, spacing=0.0):
    f, gs, cmap, hmtx, upem = font(name)
    w = 0
    for ch in text:
        w += hmtx[cmap.get(ord(ch), '.notdef')][0] * size / upem + spacing
    return w - spacing if text else 0

def text(name, s, size, x, y, fill, spacing=0.0, anchor='start', extra=''):
    """A <path> for `s` with its baseline at y."""
    f, gs, cmap, hmtx, upem = font(name)
    width = measure(name, s, size, spacing)
    if anchor == 'end': x -= width
    elif anchor == 'middle': x -= width / 2
    k = size / upem
    pen = SVGPathPen(gs, ntos=lambda v: ('%.2f' % v).rstrip('0').rstrip('.'))
    cx = x
    for ch in s:
        g = cmap.get(ord(ch), '.notdef')
        gs[g].draw(TransformPen(pen, (k, 0, 0, -k, cx, y)))
        cx += hmtx[g][0] * k + spacing
    return f'<path d="{pen.getCommands()}" fill="{fill}"{extra}/>'

def svg(w, h, body, title):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" role="img">'
            f'<title>{title}</title>{body}</svg>\n')

def save(name, content):
    with open(os.path.join(OUT, name), 'w') as f: f.write(content)

# ---- shared: the backdrop
def backdrop(w, h, seed=3, light=True):
    """Defs and rects for the title screen's backdrop, w x h."""
    d = f'''<defs>
<linearGradient id="bg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#77746E"/><stop offset="1" stop-color="#2E2D2B"/></linearGradient>
<radialGradient id="lit" cx="0.72" cy="0.12" r="0.55" gradientTransform="translate(0.72 0.12) scale(1 {w/h*0.6:.3f}) translate(-0.72 -0.12)"><stop offset="0" stop-color="#FFFFFF" stop-opacity="0.32"/><stop offset="1" stop-color="#FFFFFF" stop-opacity="0"/></radialGradient>
<radialGradient id="glow" cx="0.0" cy="1.05" r="0.6" gradientTransform="translate(0 1.05) scale(1 {w/h*0.55:.3f}) translate(0 -1.05)"><stop offset="0" stop-color="#F2A50E" stop-opacity="0.95"/><stop offset="0.35" stop-color="#C8820A" stop-opacity="0.45"/><stop offset="1" stop-color="#C8820A" stop-opacity="0"/></radialGradient>
<radialGradient id="vig" cx="0.5" cy="0.5" r="0.75"><stop offset="0.55" stop-color="#000" stop-opacity="0"/><stop offset="1" stop-color="#000" stop-opacity="0.55"/></radialGradient>
<filter id="smoke" x="0" y="0" width="100%" height="100%" color-interpolation-filters="sRGB">
<feTurbulence type="fractalNoise" baseFrequency="0.0035 0.011" numOctaves="5" seed="{seed}" result="n"/>
<feColorMatrix in="n" type="matrix" values="0 0 0 0 1  0 0 0 0 1  0 0 0 0 0.98  1.6 0 0 0 -0.62"/>
</filter>
<filter id="smoke2" x="0" y="0" width="100%" height="100%" color-interpolation-filters="sRGB">
<feTurbulence type="fractalNoise" baseFrequency="0.009 0.02" numOctaves="4" seed="{seed+40}" result="n"/>
<feColorMatrix in="n" type="matrix" values="0 0 0 0 0  0 0 0 0 0  0 0 0 0 0  1.4 0 0 0 -0.6"/>
</filter>
</defs>'''
    d += f'<rect width="{w}" height="{h}" fill="url(#bg)"/>'
    d += f'<rect width="{w}" height="{h}" filter="url(#smoke2)" opacity="0.55"/>'
    d += f'<rect width="{w}" height="{h}" filter="url(#smoke)" opacity="0.5"/>'
    if light: d += f'<rect width="{w}" height="{h}" fill="url(#lit)"/>'
    d += f'<rect width="{w}" height="{h}" fill="url(#glow)"/>'
    d += f'<rect width="{w}" height="{h}" fill="url(#vig)"/>'
    return d

# ---- the hero: the title screen's atmosphere, the title at its centre
def strokes(x, y, h, colour, n=2, width=None):
    """The menu's slanted strokes, from x."""
    width = width or h * 0.16
    slant = h * 0.44
    out = ''
    for i in range(n):
        sx = x + i * width * 2
        out += f'<path d="M{sx + slant:.1f} {y}h{width:.1f}L{sx + width:.1f} {y + h}h-{width:.1f}Z" fill="{colour}"/>'
    return out

def hero(w=1280, h=440):
    b = backdrop(w, h)
    cx = w / 2
    glow = (f'<filter id="tg" x="-10%" y="-60%" width="120%" height="220%"><feGaussianBlur stdDeviation="6" result="b"/>'
            f'<feFlood flood-color="#8FE07E" flood-opacity="0.8"/><feComposite in2="b" operator="in" result="g"/>'
            f'<feMerge><feMergeNode in="g"/><feMergeNode in="g"/><feMergeNode in="SourceGraphic"/></feMerge></filter>')
    b += f'<defs>{glow}<linearGradient id="tf" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#FFFFFF"/><stop offset="1" stop-color="{GREEN}"/></linearGradient>'
    b += f'<linearGradient id="rule" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="{AMBER}" stop-opacity="0"/><stop offset="0.2" stop-color="{AMBER}"/><stop offset="0.8" stop-color="{AMBER}"/><stop offset="1" stop-color="{AMBER}" stop-opacity="0"/></linearGradient></defs>'
    size, sp = 62, 62 * 0.08
    tw = measure(TITLE, 'MODERN WARFARE 2', size, sp)
    b += f'<g filter="url(#tg)">' + text(TITLE, 'MODERN WARFARE 2', size, cx, 214, 'url(#tf)', spacing=sp, anchor='middle') + '</g>'
    b += f'<rect x="{cx - tw / 2 - 40:.0f}" y="246" width="{tw + 80:.0f}" height="2" fill="url(#rule)"/>'
    rw = measure(HEAD, 'RECOMPILED', 34, 14)
    b += text(HEAD, 'RECOMPILED', 34, cx, 296, AMBER, spacing=14, anchor='middle')
    b += strokes(cx - rw / 2 - 64, 270, 32, AMBER) + strokes(cx + rw / 2 + 24, 270, 32, AMBER)
    b += text(BODY, 'The Xbox 360 game, recompiled to run natively on Linux and Windows', 23, cx, 352, WHITE, anchor='middle', extra=' opacity="0.9"')
    save('hero.svg', svg(w, h, b, 'Modern Warfare 2, recompiled'))

# ---- section headings: a strip of the title screen's smoke that fades out
# to the right, an amber edge and the label
def heading(slug, label, seed, w=900, h=52):
    defs = (f'<defs><linearGradient id="fade" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#fff"/>'
            f'<stop offset="0.45" stop-color="#fff"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></linearGradient>'
            f'<mask id="m"><rect width="{w}" height="{h}" fill="url(#fade)"/></mask></defs>')
    b = defs + f'<g mask="url(#m)">' + backdrop(w, h, seed=seed, light=False) + f'<rect width="{w}" height="{h}" fill="#000" opacity="0.25"/></g>'
    b += f'<rect width="5" height="{h}" fill="{AMBER}"/>'
    b += text(HEAD, label, 30, 26, 36, WHITE, spacing=2.5)
    save(f'h-{slug}.svg', svg(w, h, b, label.title()))

# ---- the download button
def download(w=390, h=72):
    # the smoke, cut to a bar that ends in a slant, then the two strokes
    end, slant = w - 70, 22
    shape = f'M0 10H{end + slant}L{end} 62H0Z'
    b = f'<defs><clipPath id="bar"><path d="{shape}"/></clipPath></defs><g clip-path="url(#bar)">' + backdrop(w, h, seed=17) + f'<rect width="{w}" height="{h}" fill="#000" opacity="0.2"/></g>'
    b += f'<rect y="10" width="5" height="52" fill="{AMBER}"/>'
    b += text(MENU, 'DOWNLOAD', 40, end - 10, 50, WHITE, anchor='end')
    b += text(BODY, 'LATEST RELEASE', 14, 24, 32, AMBER, spacing=1.5)
    b += text(BODY, 'LINUX · WINDOWS', 14, 24, 50, WHITE, spacing=1.5, extra=' opacity="0.8"')
    b += strokes(end + 8, 10, 52, AMBER)
    save('download.svg', svg(w, h, b, 'Download the latest release'))

# ---- feature tiles, in a grid
FEATURES = [
    ('60+', 'FPS', 'as on the console, or uncapped'),
    ('4K', 'RESOLUTION', '480p to 2160p; 16:9, 16:10, 4:3'),
    ('1–4', 'PLAYERS', 'split screen on one PC'),
    ('STEAM', 'INVITES', 'friends join, no port to open'),
    ('LAN', 'SYSTEM LINK', 'for players without Steam'),
    ('MAPS', 'BETA', 'PC custom maps, converted'),
]
def tiles(w=900, cols=3, th=150, gap=14):
    rows = (len(FEATURES) + cols - 1) // cols
    h = rows * th + (rows - 1) * gap
    tw = (w - (cols - 1) * gap) / cols
    cells = ''.join(f'<rect x="{(i % cols) * (tw + gap):.1f}" y="{(i // cols) * (th + gap)}" width="{tw:.1f}" height="{th}"/>' for i in range(len(FEATURES)))
    # one backdrop behind every tile; the gaps show the page through
    b = f'<defs><clipPath id="cells">{cells}</clipPath></defs><g clip-path="url(#cells)">' + backdrop(w, h, seed=11) + '</g>'
    for i, (big, tag, sub) in enumerate(FEATURES):
        x = (i % cols) * (tw + gap)
        y = (i // cols) * (th + gap)
        b += f'<rect x="{x:.1f}" y="{y}" width="{tw:.1f}" height="{th}" fill="#0B0B0A" opacity="0.5"/>'
        b += f'<rect x="{x:.1f}" y="{y}" width="4" height="{th}" fill="{AMBER}"/>'
        b += text(HEAD, tag, 20, x + 24, y + 34, AMBER, spacing=2.5)
        b += text(TITLE, big, 40, x + 24, y + 90, WHITE, spacing=2)
        b += text(BODY, sub, 20, x + 24, y + 128, WHITE, extra=' opacity="0.85"')
    save('features.svg', svg(w, h, b, 'Features'))

os.makedirs(OUT, exist_ok=True)
hero()
download()
tiles()
for slug, label in [('features', 'FEATURES'), ('requirements', 'REQUIREMENTS'), ('install', 'INSTALL'),
                    ('launcher', 'THE LAUNCHER'), ('keyboard', 'KEYBOARD'), ('online', 'ONLINE'), ('safety', 'PLAYING ONLINE SAFELY'),
                    ('settings', 'SETTINGS'), ('developers', 'DEVELOPERS'), ('license', 'LICENSE')]:
    heading(slug, label, seed=len(slug) * 7 + ord(slug[0]))
