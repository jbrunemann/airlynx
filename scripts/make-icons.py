#!/usr/bin/env python3
"""Renders the AirLynx icon (all sizes, all platforms) from the glyph defined below.

Needs rsvg-convert and, for the .icns, macOS iconutil. Outputs are committed, so a normal
build does not need this script; rerun it only after changing the design.
"""
import os
import shutil
import subprocess
import tempfile

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
OUT = os.path.join(ROOT, "assets", "icon")
RES = os.path.join(ROOT, "app", "res")

NAVY = "#0B1B4D"
FUR = "#F6FAFF"
AMBER = "#FFC857"

# Lynx head, front view, drawn on a 1024 canvas inside the adaptive-icon safe zone
# (a circle of 626 px around the center). Left half is mirrored.
LEFT_HEAD = [
    (512, 352), (420, 318), (318, 196), (262, 214), (236, 420),
    (214, 528), (262, 560), (222, 640), (296, 650), (300, 722),
    (400, 760), (512, 812),
]
LEFT_EAR_INNER = [(330, 262), (368, 322), (300, 380), (286, 290)]
LEFT_TUFT = [(318, 196), (296, 110), (352, 176)]
LEFT_EYE = "M 360 520 Q 408 478 452 530 Q 410 566 360 520 Z"
LEFT_PUPIL = (408, 523, 13, 30)


def mirror(points):
    return [(1024 - x, y) for x, y in reversed(points)]


def poly(points):
    return " ".join(f"{x},{y}" for x, y in points)


def head_points():
    return LEFT_HEAD + mirror(LEFT_HEAD)[1:-1]


def glyph(fur, ink, eye, tuft=None):
    """The lynx, drawn in three colors. Pass the same color three times for a silhouette."""
    tuft = tuft or ink
    shapes = [f'<polygon points="{poly(head_points())}" fill="{fur}"/>']
    for side in (lambda p: p, mirror):
        shapes.append(f'<polygon points="{poly(side(LEFT_TUFT))}" fill="{tuft}"/>')
        shapes.append(f'<polygon points="{poly(side(LEFT_EAR_INNER))}" fill="{ink}" fill-opacity="{0.85 if tuft == ink else 1}"/>')
    # Eyes
    for flip in (False, True):
        t = 'transform="translate(1024 0) scale(-1 1)"' if flip else ""
        cx, cy, rx, ry = LEFT_PUPIL
        shapes.append(f'<g {t}><path d="{LEFT_EYE}" fill="{eye}"/>'
                      f'<ellipse cx="{cx}" cy="{cy}" rx="{rx}" ry="{ry}" fill="{ink}"/></g>')
    # Nose and mouth
    shapes.append(f'<polygon points="472,600 552,600 512,652" fill="{ink}"/>')
    shapes.append(f'<path d="M 512 652 L 512 690 M 512 690 Q 480 722 446 700 M 512 690 Q 544 722 578 700" '
                  f'fill="none" stroke="{ink}" stroke-width="12" stroke-linecap="round"/>')
    # Forehead markings: three arcs fanning out like a Wi-Fi symbol
    for r in (34, 62, 90):
        a = 0.82
        x0, x1 = 512 - r * a, 512 + r * a
        y = 456 - r * 0.58
        shapes.append(f'<path d="M {x0:.0f} {y:.0f} A {r} {r} 0 0 1 {x1:.0f} {y:.0f}" fill="none" '
                      f'stroke="{ink}" stroke-width="13" stroke-linecap="round"/>')
    shapes.append(f'<circle cx="512" cy="{456}" r="12" fill="{ink}"/>')
    return "\n".join(shapes)


def svg(body, defs=""):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">'
            f"<defs>{defs}</defs>{body}</svg>")


BG_GRADIENT = ('<linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">'
               '<stop offset="0" stop-color="#2D3FD0"/><stop offset="1" stop-color="#14B8D4"/></linearGradient>')


def scaled(body, scale, dy=0):
    return f'<g transform="translate(512 {512 + dy}) scale({scale}) translate(-512 -512)">{body}</g>'


def full_icon():
    """macOS style: rounded square on a transparent 1024 canvas with the usual margin."""
    body = (f'<rect x="100" y="100" width="824" height="824" rx="185" fill="url(#bg)"/>'
            + scaled(glyph(FUR, NAVY, AMBER), 1.0, dy=18))
    return svg(body, BG_GRADIENT)


def logo():
    """The in-app mark: the rounded square cropped to its edges, so the UI can size it exactly."""
    return full_icon().replace('viewBox="0 0 1024 1024"', 'viewBox="100 100 824 824"')


def adaptive_background():
    return svg('<rect width="1024" height="1024" fill="url(#bg)"/>', BG_GRADIENT)


def adaptive_foreground():
    return svg(scaled(glyph(FUR, NAVY, AMBER), 0.86, dy=24))


def monochrome():
    """White silhouette with the face cut out, for themed icons and the notification icon."""
    cut = scaled(glyph("white", "black", "black", tuft="white"), 0.86, dy=24)
    body = f'<mask id="m"><rect width="1024" height="1024" fill="black"/>{cut}</mask>'
    return svg(f'{body}<rect width="1024" height="1024" fill="white" mask="url(#m)"/>')


def banner():
    """Android TV home screen banner, 320 by 180: the mark on the icon gradient, with the name."""
    mark = f'<g transform="translate(14 14) scale(0.15)">{glyph(FUR, NAVY, AMBER)}</g>'
    name = ('<text x="172" y="101" font-family="Helvetica Neue, Helvetica, Arial, sans-serif" font-size="36" '
            'font-weight="700" fill="white">AirLynx</text>')
    return ('<svg xmlns="http://www.w3.org/2000/svg" width="320" height="180" viewBox="0 0 320 180">'
            f'<defs>{BG_GRADIENT}</defs><rect width="320" height="180" fill="url(#bg)"/>{mark}{name}</svg>')


def render(svg_text, size, dest, height=None):
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(height or size), "-o", dest],
                   input=svg_text.encode(), check=True)


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


def main():
    write(os.path.join(OUT, "airlynx.svg"), full_icon())
    render(full_icon(), 512, os.path.join(OUT, "airlynx.png"))
    render(logo(), 192, os.path.join(OUT, "airlynx-logo.png"))

    # Android: adaptive icon (108dp canvas) at every density, plus the monochrome layer.
    for name, px in (("mdpi", 108), ("hdpi", 162), ("xhdpi", 216), ("xxhdpi", 324), ("xxxhdpi", 432)):
        render(adaptive_background(), px, os.path.join(RES, f"drawable-{name}", "ic_launcher_background.png"))
        render(adaptive_foreground(), px, os.path.join(RES, f"drawable-{name}", "ic_launcher_foreground.png"))
        render(monochrome(), px, os.path.join(RES, f"drawable-{name}", "ic_launcher_monochrome.png"))
    render(banner(), 320, os.path.join(RES, "drawable-xhdpi", "banner.png"), height=180)
    write(os.path.join(RES, "mipmap-anydpi-v26", "ic_launcher.xml"),
          '<?xml version="1.0" encoding="utf-8"?>\n'
          '<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">\n'
          '    <background android:drawable="@drawable/ic_launcher_background"/>\n'
          '    <foreground android:drawable="@drawable/ic_launcher_foreground"/>\n'
          '    <monochrome android:drawable="@drawable/ic_launcher_monochrome"/>\n'
          '</adaptive-icon>\n')

    # macOS .icns
    with tempfile.TemporaryDirectory() as tmp:
        iconset = os.path.join(tmp, "AirLynx.iconset")
        os.makedirs(iconset)
        for base in (16, 32, 128, 256, 512):
            render(full_icon(), base, os.path.join(iconset, f"icon_{base}x{base}.png"))
            render(full_icon(), base * 2, os.path.join(iconset, f"icon_{base}x{base}@2x.png"))
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", os.path.join(OUT, "AirLynx.icns")], check=True)
    shutil.rmtree(os.path.join(OUT, "__pycache__"), ignore_errors=True)


if __name__ == "__main__":
    main()
