//! `cargo xtask pages fetch`: main's newest artifacts of every source the
//! report is built from, downloaded with the gh CLI, and `sources.json`
//! naming the run behind each.

use crate::exploration::Build;
use crate::support::{at, write_json, Exit};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    env, fs,
    io::{self, Write},
    path::PathBuf,
    process::Command,
};

/// The workflow run an input came from, as `sources.json` records it.
#[derive(Serialize, Deserialize, Clone)]
pub(super) struct SourceRun {
    pub(super) run_id: u64,
    pub(super) sha: String,
    /// When the run finished, ISO 8601.
    pub(super) date: String,
    pub(super) conclusion: String,
    pub(super) artifacts: usize,
}

/// `sources.json`: the run behind each input that was found.
#[derive(Serialize, Deserialize, Default)]
pub(super) struct Sources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) site: Option<SourceRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) coverage: Option<SourceRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) properties: Option<SourceRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) fuzz: Option<SourceRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) scale: Option<SourceRun>,
}

impl Sources {
    fn slot(&mut self, key: Key) -> &mut Option<SourceRun> {
        match key {
            Key::Site => &mut self.site,
            Key::Coverage => &mut self.coverage,
            Key::Properties => &mut self.properties,
            Key::Fuzz => &mut self.fuzz,
            Key::Scale => &mut self.scale,
        }
    }
}

/// The artifacts the Pages site is built from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Key {
    Site,
    Coverage,
    Properties,
    Fuzz,
    Scale,
}

impl Key {
    fn name(self) -> &'static str {
        match self {
            Key::Site => "site",
            Key::Coverage => "coverage",
            Key::Properties => "properties",
            Key::Fuzz => "fuzz",
            Key::Scale => "scale",
        }
    }
}

/// One of the artifacts the Pages site is built from.
struct Source {
    key: Key,
    workflow: &'static str,
    /// Whether an artifact name is this source's, for the build shown.
    wanted: fn(&str, Build) -> bool,
    /// The site is required because Pages serves one deployment, so
    /// publishing without it would take the package repository offline.
    required: bool,
    /// Only a run that passed will do (a failed run's results are shown, not hidden, otherwise).
    needs_success: bool,
}

const SOURCES: [Source; 5] = [
    Source {
        key: Key::Site,
        // release.yml builds the OpenWrt package (openwrt.yml) once every
        // test passed on main.
        workflow: "release.yml",
        wanted: |n, _| n == "monokulo-openwrt-site",
        required: true,
        needs_success: true,
    },
    Source {
        key: Key::Coverage,
        workflow: "release.yml",
        wanted: |n, _| {
            n.strip_prefix("coverage-")
                .is_some_and(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        },
        required: false,
        needs_success: false,
    },
    Source {
        key: Key::Properties,
        workflow: "engine-properties.yml",
        wanted: |n, build| n.starts_with(&format!("engine-properties-{build}-")),
        required: false,
        needs_success: false,
    },
    Source {
        key: Key::Fuzz,
        workflow: "engine-fuzz.yml",
        wanted: |n, build| {
            n.starts_with("engine-fuzz-") && n.rsplit('-').nth(1) == Some(build.name())
        },
        required: false,
        needs_success: false,
    },
    Source {
        key: Key::Scale,
        workflow: "engine-scale.yml",
        wanted: |n, _| n.starts_with("engine-scale-measurements-"),
        required: false,
        needs_success: false,
    },
];
const RUNS_TO_SEARCH: u32 = 30;

#[derive(Deserialize)]
struct WorkflowRuns {
    workflow_runs: Vec<WorkflowRun>,
}

#[derive(Deserialize)]
struct WorkflowRun {
    id: u64,
    head_sha: String,
    updated_at: String,
    conclusion: Option<String>,
}

#[derive(Deserialize)]
struct Artifacts {
    artifacts: Vec<Artifact>,
}

#[derive(Deserialize)]
struct Artifact {
    name: String,
    expired: bool,
}

fn gh(args: &[&str]) -> io::Result<String> {
    let output = Command::new("gh")
        .args(args)
        .output()
        .map_err(|e| io::Error::new(e.kind(), format!("missing prerequisite: gh ({e})")))?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "gh {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn api<T: DeserializeOwned>(repo: &str, path: &str) -> io::Result<T> {
    serde_json::from_str(&gh(&["api", &format!("repos/{repo}/{path}")])?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("gh api {path}: {e}")))
}

struct FetchArgs {
    dir: PathBuf,
    repo: String,
    build: Build,
}

fn parse(args: &[&str]) -> io::Result<FetchArgs> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidInput, what.to_string());
    let [dir, options @ ..] = args else {
        return Err(bad(
            "usage: cargo xtask pages fetch DIR [--repo OWNER/NAME] [--feature zmq|default]",
        ));
    };
    let mut repo = env::var("GITHUB_REPOSITORY").ok();
    // The default build is the one that ships: zmq is a default feature.
    let mut build = Build::Default;
    let mut rest = options.iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .ok_or_else(|| bad(&format!("{flag} needs a value")))?;
        match *flag {
            "--repo" => repo = Some((*value).to_string()),
            "--feature" => {
                build = Build::parse(value).ok_or_else(|| bad("--feature is zmq or default"))?;
            }
            _ => return Err(bad(&format!("unknown option {flag}"))),
        }
    }
    Ok(FetchArgs {
        dir: PathBuf::from(dir),
        repo: repo.ok_or_else(|| bad("name the repository with --repo or GITHUB_REPOSITORY"))?,
        build,
    })
}

/// The newest finished run of a source's workflow on main that still has its
/// artifacts, and their names.
fn newest(
    repo: &str,
    source: &Source,
    build: Build,
) -> io::Result<Option<(WorkflowRun, Vec<String>)>> {
    let runs: WorkflowRuns = api(
        repo,
        &format!(
            "actions/workflows/{}/runs?branch=main&status=completed&per_page={RUNS_TO_SEARCH}",
            source.workflow
        ),
    )?;
    for run in runs.workflow_runs {
        let conclusion = run.conclusion.as_deref().unwrap_or("");
        if conclusion == "cancelled"
            || conclusion == "skipped"
            || (source.needs_success && conclusion != "success")
        {
            continue;
        }
        let artifacts: Artifacts = api(
            repo,
            &format!("actions/runs/{}/artifacts?per_page=100", run.id),
        )?;
        let names: Vec<String> = artifacts
            .artifacts
            .into_iter()
            .filter(|a| !a.expired && (source.wanted)(&a.name, build))
            .map(|a| a.name)
            .collect();
        if !names.is_empty() {
            return Ok(Some((run, names)));
        }
    }
    Ok(None)
}

/// Downloads main's newest artifacts of each source into `dir`. For each it
/// takes the newest finished run that still has them, skipping cancelled
/// runs, and writes `dir/sources.json` naming the run behind each.
pub(crate) fn fetch(args: &[&str]) -> io::Result<Exit> {
    let FetchArgs { dir, repo, build } = parse(args)?;
    let mut sources = Sources::default();
    for source in &SOURCES {
        let key = source.key.name();
        let Some((run, names)) = newest(&repo, source, build)? else {
            if source.required {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "{key}: no run of {} on main in the last {RUNS_TO_SEARCH} still has its artifact; \
                         run that workflow on main (Actions > Run workflow), then this one again",
                        source.workflow
                    ),
                ));
            }
            eprintln!("{key}: no artifacts found, skipped");
            continue;
        };
        let id = run.id.to_string();
        for name in &names {
            // One artifact goes straight in; several (the fuzz targets) get a folder each.
            let dest = if names.len() > 1 {
                dir.join(key).join(name)
            } else {
                dir.join(key)
            };
            gh(&[
                "run",
                "download",
                &id,
                "--repo",
                &repo,
                "--name",
                name,
                "--dir",
                &dest.to_string_lossy(),
            ])?;
        }
        let conclusion = run.conclusion.unwrap_or_default();
        eprintln!(
            "{key}: run {id} ({conclusion}, {}), {} artifact(s)",
            run.updated_at,
            names.len()
        );
        *sources.slot(source.key) = Some(SourceRun {
            run_id: run.id,
            sha: run.head_sha,
            date: run.updated_at,
            conclusion,
            artifacts: names.len(),
        });
    }
    fs::create_dir_all(&dir).map_err(|e| at(&dir, e))?;
    write_json(&dir.join("sources.json"), &sources)?;
    if let Ok(summary) = env::var("GITHUB_STEP_SUMMARY") {
        let mut out = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&summary)
            .map_err(|e| at(summary.as_ref(), e))?;
        writeln!(
            out,
            "### Pages sources\n\n| Source | Run | Result | Finished |\n|---|---|---|---|"
        )?;
        for source in &SOURCES {
            if let Some(run) = sources.slot(source.key) {
                writeln!(
                    out,
                    "| {} | [{id}](https://github.com/{repo}/actions/runs/{id}) | {} | {} |",
                    source.key.name(),
                    run.conclusion,
                    run.date,
                    id = run.run_id
                )?;
            }
        }
    }
    Ok(Exit::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "7e8f34dac14b985bd24a323147b71a10e2bc4b05";

    fn pick(key: Key, name: &str, build: Build) -> bool {
        (SOURCES.iter().find(|s| s.key == key).unwrap().wanted)(name, build)
    }

    #[test]
    fn the_sources_pick_only_the_artifacts_the_report_reads() {
        assert!(pick(Key::Coverage, &format!("coverage-{SHA}"), Build::Zmq));
        assert!(!pick(
            Key::Coverage,
            &format!("coverage-rust-{SHA}"),
            Build::Zmq
        ));
        assert!(pick(
            Key::Fuzz,
            "engine-fuzz-portfolio-zmq-37702639422",
            Build::Zmq
        ));
        assert!(!pick(
            Key::Fuzz,
            "engine-fuzz-portfolio-default-37702639422",
            Build::Zmq
        ));
        assert!(pick(
            Key::Fuzz,
            "engine-fuzz-portfolio-default-37702639422",
            Build::Default
        ));
        assert!(pick(
            Key::Properties,
            "engine-properties-zmq-37695839811",
            Build::Zmq
        ));
        assert!(!pick(
            Key::Properties,
            "engine-properties-default-37695839811",
            Build::Zmq
        ));
        assert!(pick(
            Key::Properties,
            "engine-properties-default-37695839811",
            Build::Default
        ));
    }

    #[test]
    fn sources_json_round_trips_and_leaves_out_what_was_not_found() {
        let mut sources = Sources::default();
        *sources.slot(Key::Coverage) = Some(SourceRun {
            run_id: 37_760_252_886,
            sha: SHA.into(),
            date: "2026-10-08T11:08:55Z".into(),
            conclusion: "success".into(),
            artifacts: 1,
        });
        let text = serde_json::to_string(&sources).unwrap();
        assert!(!text.contains("fuzz"), "{text}");
        let back: Sources = serde_json::from_str(&text).unwrap();
        assert_eq!(back.coverage.unwrap().run_id, 37_760_252_886);
        assert!(back.fuzz.is_none());
    }
}
