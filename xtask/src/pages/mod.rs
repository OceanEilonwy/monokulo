//! `cargo xtask pages build`: the coverage report (GitHub Pages: /testing/)
//! from CI artifacts, `cargo xtask pages fetch`: those artifacts, downloaded
//! from main's newest runs, and `cargo xtask serve`: a built site, served to
//! look at.
//!
//! Every input of the report is optional; the pages say what they have no
//! data for. A build reads the artifacts (`inputs`), works out what the
//! pages show (`model`) and renders a page per section (`render`), beside
//! the site's stylesheet and the report's script (in the site's `assets/`, one
//! level up, which every part shares: `site_assets`), the screenshot
//! gallery (`gallery/`), the report pages the pages link and what those
//! need (`reports/`) and a shields.io endpoint for the coverage badge
//! (`badge.json`). Only one engine build of the property and fuzz runs is
//! shown (ZMQ unless told otherwise): they run each build separately.

mod api_reference;
mod docs;
mod fetch;
mod format;
mod inputs;
mod install;
mod landing;
mod model;
mod render;
mod serve;
mod shell;
mod site_assets;
pub(crate) mod site_header;
#[cfg(test)]
mod tests;

pub(crate) use api_reference::build as api;
pub(crate) use docs::build as docs;
pub(crate) use fetch::fetch;
pub(crate) use inputs::{junit, Test, TestStatus};
pub(crate) use landing::build as landing;
pub(crate) use serve::serve;

use crate::exploration::Build;
use crate::support::{at, read_json, write_json, Exit};
use fetch::Sources;
use inputs::Coverage;
use model::Report;
use serde::Serialize;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// shields.io colours by line coverage, as the Rust ecosystem's badges band it.
const BADGE_BANDS: [(f64, &str); 3] = [(90.0, "brightgreen"), (80.0, "green"), (70.0, "yellow")];
const BADGE_BELOW_BANDS: &str = "orange";
const BADGE_UNKNOWN: &str = "lightgrey";
/// What a build writes into `--out`, and so what a rebuild replaces there.
const OUTPUTS: [&str; 11] = [
    "index.html",
    "tests.html",
    "coverage.html",
    "properties.html",
    "fuzzing.html",
    "scale.html",
    "live.html",
    "screens.html",
    "badge.json",
    "gallery",
    "reports",
];

pub(crate) const HELP: &str = "\
        pages build --out DIR [--coverage DIR] [--properties DIR] [--fuzz DIR] [--scale DIR]\n\
                     [--live DIR] [--sources FILE] [--feature zmq|default] [--github-url URL]\n\
                      Build the coverage report (GitHub Pages /testing/) from CI artifacts: the joined\n\
                      coverage artifact (or target/coverage), an engine-properties artifact, a folder of\n\
                      engine-fuzz artifacts, an engine-scale-measurements artifact, a live-network artifact,\n\
                      and a JSON file naming the run behind each; replaces its own files\n\
                      in DIR\n\
        pages fetch DIR [--repo OWNER/NAME] [--feature zmq|default]\n\
                      Download main's newest OpenWrt apk repository, coverage, property, fuzz, scale and live-network\n\
                      artifacts\n\
                      into DIR, with DIR/sources.json naming their runs (needs the gh CLI)\n\
        pages landing --site DIR [--github-url URL]\n\
                      Build the landing page (GitHub Pages /), about the product, into DIR\n\
        pages docs --site DIR [--site-url URL] [--github-url URL]\n\
                      Build the docs into the site DIR from xtask/src/pages/docs/*.md and the install\n\
                      pages (xtask/src/pages/install/): /introduction/, a folder per page beside it,\n\
                      /api/, the Geomart sample shop (examples/geomart/) under /geomart/, and a redirect\n\
                      from each page's old address under /docs/. DIR holds the OpenWrt apk repositories\n\
                      under openwrt/, which the router's page names, at --site-url\n\
                      (https://docs.monokulo.com by default), and publishes the key that signs them;\n\
                      the install pages' releases and every page's edit link are --github-url's\n\
        pages api --site DIR\n\
                      Build the HTTP API's reference into the site DIR, generated from the code and\n\
                      from exchanges recorded against a test engine (runs monokulo's api_reference example):\n\
                      /api/reference/, /rels/{rel}/, /problems/{slug}/ and /api/v1/openapi.json\n\
        serve DIR [PORT]\n\
                      Serve DIR on http://127.0.0.1:PORT (8000) to look at a built site as Pages serves it";

/// The shields.io endpoint the README's coverage badge reads.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Badge {
    schema_version: u8,
    label: &'static str,
    message: String,
    color: &'static str,
}

fn badge(shipping: Coverage) -> Badge {
    let (message, color) = match format::share(shipping.lines.covered, shipping.lines.total) {
        Some(pct) => (
            format!("{pct:.1}%"),
            BADGE_BANDS
                .iter()
                .find(|(floor, _)| pct >= *floor)
                .map_or(BADGE_BELOW_BANDS, |(_, colour)| colour),
        ),
        None => ("unknown".to_string(), BADGE_UNKNOWN),
    };
    Badge {
        schema_version: 1,
        label: "coverage",
        message,
        color,
    }
}

struct SiteArgs {
    out: PathBuf,
    coverage: Option<PathBuf>,
    properties: Option<PathBuf>,
    fuzz: Option<PathBuf>,
    scale: Option<PathBuf>,
    live: Option<PathBuf>,
    sources: Option<PathBuf>,
    build: Build,
    github_url: String,
}

fn parse(args: &[&str]) -> io::Result<SiteArgs> {
    let bad = |what: String| io::Error::new(io::ErrorKind::InvalidInput, what);
    let (mut out, mut coverage, mut properties, mut fuzz, mut scale, mut live, mut sources) =
        (None, None, None, None, None, None, None);
    // The default build is the one that ships: zmq is a default feature.
    let mut build = Build::Default;
    let mut github_url = site_header::GITHUB_URL.to_string();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .ok_or_else(|| bad(format!("{flag} needs a value")))?;
        let path = Some(PathBuf::from(value));
        match *flag {
            "--out" => out = path,
            "--coverage" => coverage = path,
            "--properties" => properties = path,
            "--fuzz" => fuzz = path,
            "--scale" => scale = path,
            "--live" => live = path,
            "--sources" => sources = path,
            "--feature" => {
                build =
                    Build::parse(value).ok_or_else(|| bad("--feature is zmq or default".into()))?;
            }
            "--github-url" => github_url = (*value).trim_end_matches('/').to_string(),
            _ => return Err(bad(format!("unknown option {flag}"))),
        }
    }
    Ok(SiteArgs {
        out: out.ok_or_else(|| bad("pages build needs --out DIR".into()))?,
        coverage,
        properties,
        fuzz,
        scale,
        live,
        sources,
        build,
        github_url,
    })
}

/// Removes what an earlier build wrote into `out`, and nothing else.
fn clear(out: &Path) -> io::Result<()> {
    for name in OUTPUTS {
        let path = out.join(name);
        if path.is_dir() {
            fs::remove_dir_all(&path).map_err(|e| at(&path, e))?;
        } else if path.exists() {
            fs::remove_file(&path).map_err(|e| at(&path, e))?;
        }
    }
    fs::create_dir_all(out).map_err(|e| at(out, e))
}

/// Reads every input given, converting the screenshots into `out`, and
/// copies the report pages they link.
fn read_inputs(args: &SiteArgs, out: &Path) -> io::Result<Report> {
    let sources: Sources = match &args.sources {
        Some(path) => read_json(path)?,
        None => Sources::default(),
    };
    let properties = match &args.properties {
        Some(src) => inputs::properties(src, args.build)?,
        None => None,
    };
    let Some(src) = &args.coverage else {
        let mut report = Report::new(args.github_url.clone(), sources, None, properties);
        read_nightly(args, &mut report)?;
        return Ok(report);
    };
    let (coverage, mut linked) = inputs::coverage(src)?;
    let mut report = Report::new(args.github_url.clone(), sources, Some(coverage), properties);
    let run = src.join("stress/run.json");
    if run.is_file() {
        report.stress = Some(inputs::stress(&run)?);
    }
    if let Some((gallery, reports)) = inputs::gallery(src, out)? {
        report.gallery = Some(gallery);
        linked.extend(reports);
    }
    inputs::ship_reports(src, out, &linked)?;
    read_nightly(args, &mut report)?;
    Ok(report)
}

/// The fuzz, scale and live-network runs.
fn read_nightly(args: &SiteArgs, report: &mut Report) -> io::Result<()> {
    if let Some(src) = &args.fuzz {
        report.fuzz = inputs::fuzz(src, args.build)?;
    }
    if let Some(src) = &args.scale {
        if let Some(run) = inputs::report_files(src)?
            .into_iter()
            .find(|p| p.ends_with("run.json"))
        {
            report.scale = Some(inputs::stress(&run)?);
        }
    }
    if let Some(src) = &args.live {
        report.live = inputs::live(src)?;
    }
    Ok(())
}

/// Builds the report into `--out`, replacing what an earlier build put there
/// and nothing else.
pub(crate) fn build(args: &[&str]) -> io::Result<Exit> {
    let args = parse(args)?;
    let out = &args.out;
    clear(out)?;
    let report = read_inputs(&args, out)?;
    let pages = render::write(&report, out)?;
    let shipping = report
        .coverage
        .as_ref()
        .map_or_else(Coverage::default, |c| c.shipping);
    write_json(&out.join("badge.json"), &badge(shipping))?;
    site_assets::write(&out.join(".."))?;
    let have: Vec<&str> = [
        ("coverage", report.coverage.is_some()),
        ("properties", report.properties.is_some()),
        ("fuzz", !report.fuzz.is_empty()),
        ("scale", report.scale.is_some()),
        ("live", report.live.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, present)| present.then_some(name))
    .collect();
    eprintln!(
        "coverage report in {}: {} page(s), {}",
        out.display(),
        pages.len(),
        if have.is_empty() {
            "no data".into()
        } else {
            have.join(", ")
        }
    );
    Ok(Exit::SUCCESS)
}
