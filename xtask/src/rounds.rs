//! `cargo xtask stress rounds`: the round length sweep (docs/engine_stress.md).
//!
//! Runs `round_sweep` once for each scenario of
//! `xtask/stress/round_sweep_v2.json` at each round length, pinned to one
//! CPU as the capacity sweep is, and writes `run.json` and `index.html` to
//! `target/coverage/stress-rounds`. The binary is built in release: the
//! sweep weighs scan time against link time, and a debug build's scan time
//! isn't the engine's.

use std::{
    fs, io,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::stress::{atomic_json, fresh_db, profile, target_dir};
use crate::{escape_html, root};

/// The columns of each scenario's table: field, heading, decimals.
const COLUMNS: [(&str, &str, usize); 9] = [
    ("blocks_per_sec", "Blocks/s", 1),
    ("refetch_ratio", "Sent per distinct", 2),
    ("blocks_scanned", "Scanned", 0),
    ("blocks_served", "Sent", 0),
    ("discarded_cache_bytes", "Discarded unread (bytes)", 0),
    ("rounds", "Rounds", 0),
    ("round_ms_p50", "Round p50 (ms)", 0),
    ("round_ms_max", "Longest round (ms)", 0),
    ("idle_round_us_p50", "Idle round (µs)", 0),
];

fn number(value: &Value, decimals: usize) -> String {
    value
        .as_f64()
        .map(|n| format!("{n:.decimals$}"))
        .unwrap_or_else(|| "–".into())
}

pub fn run() -> io::Result<bool> {
    let output = root().join("target/coverage/stress-rounds");
    if output.exists() {
        fs::remove_dir_all(&output)?;
    }
    fs::create_dir_all(&output)?;
    let raw = fs::read(root().join("xtask/stress/round_sweep_v2.json"))?;
    let scenario: Value = serde_json::from_slice(&raw)?;
    let checksum = hex::encode(Sha256::digest(&raw));
    let built = Command::new("cargo")
        .args([
            "build",
            "--release",
            "-p",
            "engine",
            "--bin",
            "round_sweep",
            "--locked",
        ])
        .current_dir(root())
        .status()?;
    if !built.success() {
        return Err(io::Error::other("round sweep build failed"));
    }
    let binary = target_dir().join("release/round_sweep");
    let hardware = profile(&output);
    atomic_json(&output.join("hardware.json"), &hardware)?;
    let cpu = hardware["selected_cpu"].as_u64().unwrap_or(0);
    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let budgets = scenario["round_budgets_ms"]
        .as_array()
        .ok_or_else(|| io::Error::other("scenario has no round budgets"))?;
    let scenarios = scenario["scenarios"]
        .as_array()
        .ok_or_else(|| io::Error::other("scenario has no scenarios"))?;

    let mut results = Vec::new();
    let mut ok = true;
    for case in scenarios {
        let name = case["name"].as_str().unwrap_or("unnamed");
        for budget in budgets {
            let label = format!("{name}-{budget}ms");
            let db = fresh_db(&output, &label);
            let mut args = vec![
                "--db".to_owned(),
                db.to_string_lossy().into_owned(),
                "--round-budget-ms".into(),
                budget.to_string(),
            ];
            for (flag, value) in [
                ("--tenants", &case["tenants"]),
                ("--groups", &case["groups"]),
                ("--backlog-blocks", &case["backlog_blocks"]),
                ("--link-kbps", &case["link_kbps"]),
                ("--block-bytes", &scenario["block_bytes"]),
                ("--rtt-ms", &case["rtt_ms"]),
                ("--ttfb-us-per-block", &scenario["ttfb_us_per_block"]),
                ("--budget-mb", &scenario["budget_mb"]),
                ("--idle-rounds", &scenario["idle_rounds"]),
                ("--max-secs", &scenario["max_secs"]),
                ("--seed", &scenario["seed"]),
            ] {
                args.push(flag.into());
                args.push(value.to_string());
            }
            eprintln!("round sweep: {label}");
            let mut command = if cfg!(target_os = "linux") {
                let mut command = Command::new("taskset");
                command.arg("-c").arg(cpu.to_string()).arg(&binary);
                command
            } else {
                Command::new(&binary)
            };
            let run = command
                .args(&args)
                .current_dir(root())
                .stdin(Stdio::null())
                .output()?;
            fs::write(output.join(format!("{label}.log")), &run.stderr)?;
            let point: Value = serde_json::from_slice(&run.stdout).unwrap_or(Value::Null);
            let success =
                run.status.success() && point["schema_version"] == scenario["schema_version"];
            ok &= success;
            for suffix in ["", "-wal", "-shm"] {
                let _ = fs::remove_file(format!("{}{suffix}", db.display()));
            }
            results.push(json!({"scenario": name, "label": label, "ok": success, "point": point}));
            atomic_json(
                &output.join("run.json"),
                &json!({"started_at_utc": started_at, "scenario": scenario,
                    "scenario_checksum": checksum, "hardware": hardware, "results": results}),
            )?;
        }
    }
    report(&output, scenarios, &results)?;
    Ok(ok)
}

fn report(output: &std::path::Path, scenarios: &[Value], results: &[Value]) -> io::Result<()> {
    let mut page = String::from("<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Round length sweep</title><style>body{font:16px/1.5 system-ui,sans-serif;max-width:1100px;margin:1rem auto;padding:0 1rem;color:#17212b}table{border-collapse:collapse;width:100%}th,td{border-bottom:1px solid #ccd3db;text-align:right;padding:.4rem}th:first-child,td:first-child{text-align:left}.bad{color:#a32}</style><h1>Round length sweep</h1><p>Catch-up through a backlog of blocks at each round length (docs/engine_stress.md). \"Sent per distinct\" above 1 is blocks the node sent again: fetched ahead, then dropped before their scan. Blocks/s counts each group's blocks, averaged over its tenants.</p>");
    for case in scenarios {
        let name = case["name"].as_str().unwrap_or("unnamed");
        page.push_str(&format!(
            "<h2>{}</h2><p>{} kbit/s, {} ms round trip, {} tenants in {} groups, {} blocks behind.</p><table><tr><th>Round</th>",
            escape_html(name),
            case["link_kbps"],
            case["rtt_ms"],
            case["tenants"],
            case["groups"],
            case["backlog_blocks"]
        ));
        for (_, heading, _) in COLUMNS {
            page.push_str(&format!("<th>{}</th>", escape_html(heading)));
        }
        page.push_str("</tr>");
        for result in results.iter().filter(|r| r["scenario"] == name) {
            let point = &result["point"];
            let class = if result["ok"] == true && point["drained"] == true {
                ""
            } else {
                " class=\"bad\""
            };
            page.push_str(&format!(
                "<tr{class}><td>{} ms{}</td>",
                point["round_budget_ms"],
                if point["drained"] == true {
                    ""
                } else {
                    " (not drained)"
                }
            ));
            for (field, _, decimals) in COLUMNS {
                page.push_str(&format!("<td>{}</td>", number(&point[field], decimals)));
            }
            page.push_str("</tr>");
        }
        page.push_str("</table>");
    }
    page.push_str(
        "<p><a href=\"run.json\">Run data</a> · <a href=\"hardware.json\">Hardware</a></p></html>",
    );
    fs::write(output.join("index.html"), page)
}
