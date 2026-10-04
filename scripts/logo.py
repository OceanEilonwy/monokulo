#!/usr/bin/env python3
"""Draws the Monokulo mark and writes every copy of it in the repo.

The mark is a monocle whose lens is cut like a stone, the facets laid out as
a curve tree (the structure FCMP++ proves membership in): a hexagon root,
six branches, twelve limbs and twelve leaves. At rest every facet is solid
orange. The loading version lights one leaf-to-root path at a time, in a
scrambled order, the way a membership proof walks the tree without saying
which leaf.

One drawing, four renderings, picked by the size they are shown at:
  full     48px and up: facet lines in the text colour, and the chain
  small    under 48px, beside the name: no facet lines, fewer heavier links
  icon     square icons (favicon, app, plugin): no chain, the lens fills the square
  loading  the icon with the membership paths lighting in turn

Run from the repo root after changing anything here:

    python3 scripts/logo.py

and commit what it writes (the list is at the bottom of this file).
"""

import math
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# ---- geometry (viewBox 0 0 64 64) ----
CX, CY = 30.0, 28.0
RIM_R, RIM_W = 18.0, 4.5
LENS_R = RIM_R - RIM_W / 2
# The outer facets end on a 12-gon of radius 24, past the lens (its apothem,
# 23.2, clears 15.75), so the rim clips every leaf: no gap between leaf and rim.
OUTER_R = 24.0


def r2(n):
    s = f"{n:.2f}".rstrip("0").rstrip(".")
    return "0" if s == "-0" else s


def pt(r, deg):
    a = math.radians(deg)
    return (round(CX + r * math.cos(a), 2), round(CY + r * math.sin(a), 2))


def tree_layers():
    root = lambda a: pt(5, a)
    branch_tip = lambda a: pt(11, a)
    outer = lambda a: pt(OUTER_R, a)
    corners = [-90 + 60 * k for k in range(6)]
    layers = [[], [], [], []]
    layers[0].append([root(a) for a in corners])
    for a in corners:
        layers[1].append([root(a), root(a + 60), branch_tip(a + 30)])
    for a in corners:
        layers[2].append([root(a), branch_tip(a - 30), outer(a)])
        layers[2].append([root(a), outer(a), branch_tip(a + 30)])
    for b in (a + 30 for a in corners):
        layers[3].append([branch_tip(b), outer(b - 30), outer(b)])
        layers[3].append([branch_tip(b), outer(b), outer(b + 30)])
    return layers


def shares_edge(s, t):
    return sum(1 for p in s if any(abs(p[0] - q[0]) < 0.05 and abs(p[1] - q[1]) < 0.05 for q in t)) >= 2


LAYERS = tree_layers()
FACETS = [(n, pts) for n, layer in enumerate(LAYERS) for pts in layer]


def leaf_paths():
    """Each leaf's path to the root: facet indexes, leaf first."""
    paths = []
    for i, (n, pts) in enumerate(FACETS):
        if n != len(LAYERS) - 1:
            continue
        path = [i]
        for layer in range(len(LAYERS) - 2, -1, -1):
            prev = FACETS[path[-1]][1]
            path.append(next(j for j, (m, q) in enumerate(FACETS) if m == layer and shares_edge(q, prev)))
        paths.append(path)
    return paths


def d_of(pts):
    return "M" + "L".join(f"{r2(x)},{r2(y)}" for x, y in pts) + "Z"


# Root deep, branches warm, limbs mid, leaves warm: the branches match the
# leaves, so no star forms around the root at small sizes.
TONE_BY_LAYER = ["deep", "warm", "mid", "warm"]

# ---- colour sets ----
INLINE = {  # pages: colours come from theme.css roles, lines follow the text
    "ink": "currentColor",
    "mid": "var(--logo-facet-mid)",
    "warm": "var(--logo-facet-warm)",
    "deep": "var(--logo-facet-deep)",
    "glint": "var(--logo-glint)",
}
STANDALONE = {  # files: the same values theme.css gives those roles
    "ink": "#1a1917",
    "mid": "#ff6600",
    "warm": "#e85d00",
    "deep": "#c24e00",
    "glint": "#fff3e8",
}
DARK_INK = "#d4d4d4"


def pulses(windows, dur):
    """An opacity track of separate pulses, (start, end) as fractions of the cycle."""
    kt, vs = [0.0], ["0"]
    for s, e in sorted(windows):
        kt += [s, s + 0.015, e, e + 0.03]
        vs += ["0", "0.95", "0.95", "0"]
    kt.append(1.0)
    vs.append("0")
    times = ";".join(f"{min(1.0, max(0.0, t)):.3f}".rstrip("0").rstrip(".") or "0" for t in kt)
    return f'<animate attributeName="opacity" values="{";".join(vs)}" keyTimes="{times}" dur="{r2(dur)}s" repeatCount="indefinite"/>'


def loading_glow(c, edge):
    paths = leaf_paths()
    # six beats, the paths picked in golden-ratio order so consecutive beats land far apart
    order = sorted(range(len(paths)), key=lambda i: (i * 0.618) % 1)[:6]
    beat = 1 / len(order)
    windows = {}
    for k, pi in enumerate(order):
        for step, f in enumerate(paths[pi]):
            windows.setdefault(f, []).append((k * beat + step * beat * 0.13, k * beat + beat * 0.78))
    out = ""
    for f in sorted(windows):
        out += f'<path d="{d_of(FACETS[f][1])}" fill="{c["glint"]}"{edge} opacity="0">{pulses(windows[f], len(order) * 0.7)}</path>'
    return f'<g class="logo-glow">{out}</g>'


def drawing(kind, c, clip_id, rim_class=""):
    """The inside of a 64 x 64 viewBox for one rendering."""
    icon = kind in ("icon", "loading")
    small = kind == "small"
    edge = f' stroke="{c["ink"]}" stroke-width="0.55" stroke-linejoin="round"' if kind == "full" else ""
    facets = f'<circle cx="{r2(CX)}" cy="{r2(CY)}" r="{r2(LENS_R)}" fill="{c["mid"]}"/>'
    for n, pts in FACETS:
        facets += f'<path d="{d_of(pts)}" fill="{c[TONE_BY_LAYER[n]]}"{edge}/>'
    if kind == "loading":
        facets += loading_glow(c, edge)
    cls = f' class="{rim_class}"' if rim_class else ""
    body = (
        f'<defs><clipPath id="{clip_id}"><circle cx="{r2(CX)}" cy="{r2(CY)}" r="{r2(LENS_R)}"/></clipPath></defs>'
        f'<g clip-path="url(#{clip_id})">{facets}</g>'
        f'<circle{cls} cx="{r2(CX)}" cy="{r2(CY)}" r="{r2(RIM_R)}" fill="none" stroke="{c["ink"]}" stroke-width="{r2(RIM_W)}"/>'
    )
    if not icon:
        ex, ey = pt(RIM_R + 3.2, 72)
        ring_r, ring_w = (2.2, 2.2) if small else (1.9, 1.6)
        link_w, dash = (3.4, "0.1 5") if small else (2.2, "0.1 3.4")
        body += (
            f'<circle{cls} cx="{r2(ex)}" cy="{r2(ey)}" r="{r2(ring_r)}" fill="none" stroke="{c["ink"]}" stroke-width="{r2(ring_w)}"/>'
            f'<path{cls} d="M{r2(ex)},{r2(ey + 2)} C{r2(ex - 1)},{r2(ey + 10)} {r2(ex + 9)},{r2(ey + 13)} {r2(ex + 19)},{r2(ey + 6)}" '
            f'fill="none" stroke="{c["ink"]}" stroke-width="{r2(link_w)}" stroke-linecap="round" stroke-dasharray="{dash}"/>'
        )
    if icon:
        # without the chain the lens is scaled up to fill the square
        body = f'<g transform="translate(32 32) scale(1.42) translate({r2(-CX)} {r2(-CY)})">{body}</g>'
    return body


def standalone(kind, title, dark=False):
    style = ""
    rim_class = ""
    rules = []
    if dark:
        rim_class = "ink"
        rules.append(f"@media (prefers-color-scheme: dark) {{ .ink {{ stroke: {DARK_INK}; }} }}")
    if kind == "loading":
        rules.append("@media (prefers-reduced-motion: reduce) { .logo-glow { display: none; } }")
    if rules:
        style = "<style>" + " ".join(rules) + "</style>"
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">\n'
        "<!-- Generated by scripts/logo.py: edit that, not this. -->\n"
        f"<title>{title}</title>{style}"
        f'{drawing(kind, STANDALONE, "monokulo-lens", rim_class)}\n</svg>\n'
    )


GENERATED_NOTE = "Generated by scripts/logo.py: edit that, not this."


def rust_module():
    return f"""//! The Monokulo mark's drawings for inline use. {GENERATED_NOTE}
//!
//! Each is the inside of a `viewBox="0 0 64 64"` SVG. Lines are
//! `currentColor`; the facets take the `--logo-*` roles in theme.css.

/// 48px and up: facet lines and the chain.
pub const FULL: &str = r##"{drawing("full", INLINE, "monokulo-lens-full")}"##;

/// Under 48px, beside the name: no facet lines, fewer and heavier chain links.
pub const SMALL: &str = r##"{drawing("small", INLINE, "monokulo-lens-small")}"##;
"""


def ts_module():
    svg = f'<svg viewBox="0 0 64 64" aria-hidden="true" focusable="false">{drawing("loading", INLINE, "monokulo-lens-loading")}</svg>'
    return f"""// The Monokulo mark's loading drawing. {GENERATED_NOTE}
// Lines are currentColor; the facets take the --logo-* roles in theme.css.
// The `.logo-glow` group is the animation; hide it for reduced motion.
export const LOADING_MARK = '{svg}';
"""


def web_page(text):
    marks = {
        "small": drawing("small", INLINE, "monokulo-lens-small"),
        "full": drawing("full", INLINE, "monokulo-lens-full"),
    }

    def swap(m):
        return f"{m.group(1)}{marks[m.group(2)]}{m.group(3)}"

    new, count = re.subn(r"(<!-- logo:(small|full) -->.*?<svg[^>]*>).*?(</svg>)", swap, text, flags=re.S)
    assert count == 2, f"web/index.html: expected 2 logo markers, found {count}"
    return new


OUTPUTS = {
    "crates/monokulo/static/logo.svg": lambda _: standalone("full", "Monokulo"),
    "crates/monokulo/static/favicon.svg": lambda _: standalone("icon", "Monokulo", dark=True),
    "plugins/woocommerce/assets/monokulo-icon.svg": lambda _: standalone("icon", "Monokulo", dark=True),
    "crates/monokulo/src/views/logo_art.rs": lambda _: rust_module(),
    "crates/monokulo/pos-ui/src/logo.ts": lambda _: ts_module(),
    "web/index.html": web_page,
}

if __name__ == "__main__":
    for rel, render in OUTPUTS.items():
        path = ROOT / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        old = path.read_text() if path.exists() else ""
        new = render(old)
        if new != old:
            path.write_text(new)
            print(f"wrote {rel}")
        else:
            print(f"unchanged {rel}")
