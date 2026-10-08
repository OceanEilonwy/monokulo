//! `cargo xtask logo`: draws the Monokulo mark and writes every copy of it in
//! the repo.
//!
//! The mark is a monocle whose lens is cut like a stone, the facets laid out
//! as a curve tree (the structure FCMP++ proves membership in): a hexagon
//! root, six branches, twelve limbs and twelve leaves. At rest every facet is
//! solid orange. The loading version lights one leaf-to-root path at a time,
//! in a scrambled order, the way a membership proof walks the tree without
//! saying which leaf.
//!
//! One drawing, five renderings, picked by the size they are shown at:
//!   full     48px and up: facet lines in the text colour, and the chain
//!   small    under 48px, beside the name: no facet lines, fewer heavier links
//!   icon     square icons (favicon, app, plugin): no chain, the lens fills the square
//!   loading  the icon with the membership paths lighting in turn
//!   hero     full with the loading paths, for the README (light and dark files)
//!
//! Run `cargo xtask logo` after changing anything here, and commit what it
//! writes (the list is `OUTPUTS`, at the bottom).

use regex::Regex;
use std::{collections::BTreeMap, fs, io, path::Path};

// ---- geometry (viewBox 0 0 64 64) ----
const CX: f64 = 30.0;
const CY: f64 = 28.0;
const RIM_R: f64 = 18.0;
const RIM_W: f64 = 4.5;
const LENS_R: f64 = RIM_R - RIM_W / 2.0;
// The outer facets end on a 12-gon of radius 24, past the lens (its apothem,
// 23.2, clears 15.75), so the rim clips every leaf: no gap between leaf and rim.
const OUTER_R: f64 = 24.0;

type Point = (f64, f64);

/// A coordinate as the SVG carries it: two decimals at most, no trailing zeros.
fn r2(n: f64) -> String {
    let s = format!("{n:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.into()
    }
}

/// Rounded to two decimals, the way the points are kept.
fn round2(n: f64) -> f64 {
    format!("{n:.2}").parse().unwrap()
}

fn pt(r: f64, deg: f64) -> Point {
    let a = deg.to_radians();
    (round2(CX + r * a.cos()), round2(CY + r * a.sin()))
}

/// The facets, root first: each layer's triangles (and the root hexagon).
fn tree_layers() -> Vec<Vec<Vec<Point>>> {
    let root = |a: f64| pt(5.0, a);
    let branch_tip = |a: f64| pt(11.0, a);
    let outer = |a: f64| pt(OUTER_R, a);
    let corners: Vec<f64> = (0..6).map(|k| -90.0 + 60.0 * k as f64).collect();
    let mut layers = vec![Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    layers[0].push(corners.iter().map(|&a| root(a)).collect());
    for &a in &corners {
        layers[1].push(vec![root(a), root(a + 60.0), branch_tip(a + 30.0)]);
    }
    for &a in &corners {
        layers[2].push(vec![root(a), branch_tip(a - 30.0), outer(a)]);
        layers[2].push(vec![root(a), outer(a), branch_tip(a + 30.0)]);
    }
    for b in corners.iter().map(|a| a + 30.0) {
        layers[3].push(vec![branch_tip(b), outer(b - 30.0), outer(b)]);
        layers[3].push(vec![branch_tip(b), outer(b), outer(b + 30.0)]);
    }
    layers
}

fn shares_edge(s: &[Point], t: &[Point]) -> bool {
    s.iter()
        .filter(|p| {
            t.iter()
                .any(|q| (p.0 - q.0).abs() < 0.05 && (p.1 - q.1).abs() < 0.05)
        })
        .count()
        >= 2
}

/// Every facet with its layer.
fn facets() -> Vec<(usize, Vec<Point>)> {
    tree_layers()
        .into_iter()
        .enumerate()
        .flat_map(|(n, layer)| layer.into_iter().map(move |pts| (n, pts)))
        .collect()
}

/// Each leaf's path to the root: facet indexes, leaf first.
fn leaf_paths(facets: &[(usize, Vec<Point>)]) -> Vec<Vec<usize>> {
    let last = 3;
    let mut paths = Vec::new();
    for (i, (n, _)) in facets.iter().enumerate() {
        if *n != last {
            continue;
        }
        let mut path = vec![i];
        for layer in (0..last).rev() {
            let previous = &facets[*path.last().unwrap()].1;
            path.push(
                facets
                    .iter()
                    .position(|(m, q)| *m == layer && shares_edge(q, previous))
                    .unwrap(),
            );
        }
        paths.push(path);
    }
    paths
}

fn d_of(pts: &[Point]) -> String {
    format!(
        "M{}Z",
        pts.iter()
            .map(|(x, y)| format!("{},{}", r2(*x), r2(*y)))
            .collect::<Vec<_>>()
            .join("L")
    )
}

// Root deep, branches warm, limbs mid, leaves warm: the branches match the
// leaves, so no star forms around the root at small sizes.
const TONE_BY_LAYER: [&str; 4] = ["deep", "warm", "mid", "warm"];

/// A colour set: the line colour and the facet tones.
#[derive(Clone, Copy)]
struct Palette {
    ink: &'static str,
    mid: &'static str,
    warm: &'static str,
    deep: &'static str,
    glint: &'static str,
}

impl Palette {
    fn tone(&self, name: &str) -> &'static str {
        match name {
            "deep" => self.deep,
            "warm" => self.warm,
            _ => self.mid,
        }
    }
}

/// Pages: colours come from theme.css roles, lines follow the text.
const INLINE: Palette = Palette {
    ink: "currentColor",
    mid: "var(--logo-facet-mid)",
    warm: "var(--logo-facet-warm)",
    deep: "var(--logo-facet-deep)",
    glint: "var(--logo-glint)",
};
/// Files: the same values theme.css gives those roles.
const STANDALONE: Palette = Palette {
    ink: "#1a1917",
    mid: "#ff6600",
    warm: "#e85d00",
    deep: "#c24e00",
    glint: "#fff3e8",
};
const DARK_INK: &str = "#d4d4d4";
/// The README's logo on GitHub dark.
const ON_DARK: Palette = Palette {
    ink: DARK_INK,
    ..STANDALONE
};

/// A key time as the animation carries it: three decimals at most.
fn r3(t: f64) -> String {
    let s = format!("{:.3}", t.clamp(0.0, 1.0));
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() {
        "0".into()
    } else {
        s.into()
    }
}

/// An opacity track of separate pulses, (start, end) as fractions of the cycle.
fn pulses(windows: &[(f64, f64)], dur: f64) -> String {
    let mut sorted = windows.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let (mut times, mut values) = (vec![0.0], vec!["0"]);
    for (s, e) in sorted {
        times.extend([s, s + 0.015, e, e + 0.03]);
        values.extend(["0", "0.95", "0.95", "0"]);
    }
    times.push(1.0);
    values.push("0");
    let times: Vec<String> = times.into_iter().map(r3).collect();
    format!(
        r#"<animate attributeName="opacity" values="{}" keyTimes="{}" dur="{}s" repeatCount="indefinite"/>"#,
        values.join(";"),
        times.join(";"),
        r2(dur)
    )
}

fn loading_glow(c: &Palette, edge: &str, facets: &[(usize, Vec<Point>)]) -> String {
    let paths = leaf_paths(facets);
    // six beats, the paths picked in golden-ratio order so consecutive beats land far apart
    let mut order: Vec<usize> = (0..paths.len()).collect();
    order.sort_by(|a, b| {
        ((*a as f64 * 0.618) % 1.0)
            .partial_cmp(&((*b as f64 * 0.618) % 1.0))
            .unwrap()
    });
    order.truncate(6);
    let beat = 1.0 / order.len() as f64;
    let mut windows: BTreeMap<usize, Vec<(f64, f64)>> = BTreeMap::new();
    for (k, &pi) in order.iter().enumerate() {
        for (step, &f) in paths[pi].iter().enumerate() {
            let k = k as f64;
            windows
                .entry(f)
                .or_default()
                .push((k * beat + step as f64 * beat * 0.13, k * beat + beat * 0.78));
        }
    }
    let mut out = String::new();
    for (f, w) in &windows {
        out += &format!(
            r#"<path d="{}" fill="{}"{edge} opacity="0">{}</path>"#,
            d_of(&facets[*f].1),
            c.glint,
            pulses(w, order.len() as f64 * 0.7)
        );
    }
    format!(r#"<g class="logo-glow">{out}</g>"#)
}

/// The inside of a 64 x 64 viewBox for one rendering.
fn drawing(kind: &str, c: &Palette, clip_id: &str, rim_class: &str) -> String {
    let icon = matches!(kind, "icon" | "loading");
    let small = kind == "small";
    let edge = if matches!(kind, "full" | "hero") {
        format!(
            r#" stroke="{}" stroke-width="0.55" stroke-linejoin="round""#,
            c.ink
        )
    } else {
        String::new()
    };
    let all = facets();
    let mut drawn = format!(
        r#"<circle cx="{}" cy="{}" r="{}" fill="{}"/>"#,
        r2(CX),
        r2(CY),
        r2(LENS_R),
        c.mid
    );
    for (n, pts) in &all {
        drawn += &format!(
            r#"<path d="{}" fill="{}"{edge}/>"#,
            d_of(pts),
            c.tone(TONE_BY_LAYER[*n])
        );
    }
    if matches!(kind, "loading" | "hero") {
        drawn += &loading_glow(c, &edge, &all);
    }
    let cls = if rim_class.is_empty() {
        String::new()
    } else {
        format!(r#" class="{rim_class}""#)
    };
    let mut body = format!(
        r#"<defs><clipPath id="{clip_id}"><circle cx="{cx}" cy="{cy}" r="{lens}"/></clipPath></defs><g clip-path="url(#{clip_id})">{drawn}</g><circle{cls} cx="{cx}" cy="{cy}" r="{rim}" fill="none" stroke="{ink}" stroke-width="{rim_w}"/>"#,
        cx = r2(CX),
        cy = r2(CY),
        lens = r2(LENS_R),
        rim = r2(RIM_R),
        ink = c.ink,
        rim_w = r2(RIM_W)
    );
    if !icon {
        let (ex, ey) = pt(RIM_R + 3.2, 72.0);
        let (ring_r, ring_w) = if small { (2.2, 2.2) } else { (1.9, 1.6) };
        let (link_w, dash) = if small {
            (3.4, "0.1 5")
        } else {
            (2.2, "0.1 3.4")
        };
        body += &format!(
            r#"<circle{cls} cx="{}" cy="{}" r="{}" fill="none" stroke="{ink}" stroke-width="{}"/><path{cls} d="M{},{} C{},{} {},{} {},{}" fill="none" stroke="{ink}" stroke-width="{}" stroke-linecap="round" stroke-dasharray="{dash}"/>"#,
            r2(ex),
            r2(ey),
            r2(ring_r),
            r2(ring_w),
            r2(ex),
            r2(ey + 2.0),
            r2(ex - 1.0),
            r2(ey + 10.0),
            r2(ex + 9.0),
            r2(ey + 13.0),
            r2(ex + 19.0),
            r2(ey + 6.0),
            r2(link_w),
            ink = c.ink
        );
    }
    if icon {
        // without the chain the lens is scaled up to fill the square
        body = format!(
            r#"<g transform="translate(32 32) scale(1.42) translate({} {})">{body}</g>"#,
            r2(-CX),
            r2(-CY)
        );
    }
    body
}

const GENERATED_NOTE: &str =
    "Generated by `cargo xtask logo` (xtask/src/logo.rs): edit that, not this.";

fn standalone(kind: &str, title: &str, dark: bool, palette: &Palette) -> String {
    let mut rules = Vec::new();
    let rim_class = if dark {
        rules.push(format!(
            "@media (prefers-color-scheme: dark) {{ .ink {{ stroke: {DARK_INK}; }} }}"
        ));
        "ink"
    } else {
        ""
    };
    if matches!(kind, "loading" | "hero") {
        rules.push(
            "@media (prefers-reduced-motion: reduce) { .logo-glow { display: none; } }".into(),
        );
    }
    let style = if rules.is_empty() {
        String::new()
    } else {
        format!("<style>{}</style>", rules.join(" "))
    };
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\">\n<!-- {GENERATED_NOTE} -->\n<title>{title}</title>{style}{}\n</svg>\n",
        drawing(kind, palette, "monokulo-lens", rim_class)
    )
}

fn rust_module() -> String {
    format!(
        "//! The Monokulo mark's drawings for inline use. {GENERATED_NOTE}\n//!\n//! Each is the inside of a `viewBox=\"0 0 64 64\"` SVG. Lines are\n//! `currentColor`; the facets take the `--logo-*` roles in theme.css.\n\n/// 48px and up: facet lines and the chain.\npub const FULL: &str = r##\"{}\"##;\n\n/// Under 48px, beside the name: no facet lines, fewer and heavier chain links.\npub const SMALL: &str = r##\"{}\"##;\n",
        drawing("full", &INLINE, "monokulo-lens-full", ""),
        drawing("small", &INLINE, "monokulo-lens-small", "")
    )
}

fn ts_module() -> String {
    let svg = format!(
        r#"<svg viewBox="0 0 64 64" aria-hidden="true" focusable="false">{}</svg>"#,
        drawing("loading", &INLINE, "monokulo-lens-loading", "")
    );
    format!(
        "// The Monokulo mark's loading drawing. {GENERATED_NOTE}\n// Lines are currentColor; the facets take the --logo-* roles in theme.css.\n// The `.logo-glow` group is the animation; hide it for reduced motion.\nexport const LOADING_MARK = '{svg}';\n"
    )
}

/// A page with `<!-- logo:small -->` / `<!-- logo:full -->` markers before
/// inline SVGs: the drawings inside them, replaced.
fn web_page(text: &str, markers: usize, name: &str) -> io::Result<String> {
    let marker = Regex::new(r"(?s)(<!-- logo:(small|full) -->.*?<svg[^>]*>).*?(</svg>)").unwrap();
    let count = marker.find_iter(text).count();
    if count != markers {
        return Err(io::Error::other(format!(
            "{name}: expected {markers} logo markers, found {count}"
        )));
    }
    Ok(marker
        .replace_all(text, |c: &regex::Captures| {
            let mark = if &c[2] == "small" {
                drawing("small", &INLINE, "monokulo-lens-small", "")
            } else {
                drawing("full", &INLINE, "monokulo-lens-full", "")
            };
            format!("{}{mark}{}", &c[1], &c[3])
        })
        .into_owned())
}

/// Every file the mark is written to, from what the file holds now.
fn render(path: &str, old: &str) -> io::Result<String> {
    Ok(match path {
        "crates/monokulo/static/logo.svg" => standalone("full", "Monokulo", false, &STANDALONE),
        "docs/readme/logo.svg" => standalone("hero", "Monokulo", false, &STANDALONE),
        "docs/readme/logo-dark.svg" => standalone("hero", "Monokulo", false, &ON_DARK),
        "crates/monokulo/static/favicon.svg" | "plugins/woocommerce/assets/monokulo-icon.svg" => {
            standalone("icon", "Monokulo", true, &STANDALONE)
        }
        "crates/monokulo/src/views/logo_art.rs" => rust_module(),
        "crates/monokulo/pos-ui/src/logo.ts" => ts_module(),
        "web/index.html" => web_page(old, 2, path)?,
        "web/pages/quality/index.html" => web_page(old, 1, path)?,
        _ => unreachable!("{path} is not a logo output"),
    })
}

const OUTPUTS: [&str; 9] = [
    "crates/monokulo/static/logo.svg",
    "docs/readme/logo.svg",
    "docs/readme/logo-dark.svg",
    "crates/monokulo/static/favicon.svg",
    "plugins/woocommerce/assets/monokulo-icon.svg",
    "crates/monokulo/src/views/logo_art.rs",
    "crates/monokulo/pos-ui/src/logo.ts",
    "web/index.html",
    "web/pages/quality/index.html",
];

/// Writes every output under `root` that differs from what the mark draws.
pub(crate) fn write(root: &Path) -> io::Result<bool> {
    for rel in OUTPUTS {
        let path = root.join(rel);
        let old = fs::read_to_string(&path).unwrap_or_default();
        let new = render(rel, &old)?;
        if new != old {
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(&path, new)?;
            println!("wrote {rel}");
        } else {
            println!("unchanged {rel}");
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_copy_of_the_mark_is_up_to_date() {
        let root = crate::root();
        for rel in OUTPUTS {
            let old = fs::read_to_string(root.join(rel)).unwrap();
            assert!(
                render(rel, &old).unwrap() == old,
                "{rel} is stale: run `cargo xtask logo`"
            );
        }
    }

    #[test]
    fn the_tree_has_a_root_six_branches_twelve_limbs_and_twelve_leaves() {
        let layers = tree_layers();
        assert_eq!(
            layers.iter().map(Vec::len).collect::<Vec<_>>(),
            [1, 6, 12, 12]
        );
        let all = facets();
        for path in leaf_paths(&all) {
            assert_eq!(
                path.iter().map(|f| all[*f].0).collect::<Vec<_>>(),
                [3, 2, 1, 0]
            );
        }
    }

    #[test]
    fn numbers_drop_trailing_zeros_and_negative_zero() {
        assert_eq!(
            [r2(30.0), r2(1.5), r2(-0.001), r2(4.25)],
            ["30", "1.5", "0", "4.25"]
        );
        assert_eq!([r3(0.0), r3(1.2), r3(0.25)], ["0", "1", "0.25"]);
    }
}
