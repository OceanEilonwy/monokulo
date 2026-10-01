use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::Path,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{escape_html, root};

/// The engine the report measures when no driver is named.
pub const DEFAULT_DRIVER: &str = "scheduler";

fn read(path: &str) -> String {
    fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn command(name: &str, args: &[&str]) -> String {
    Command::new(name)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}

fn first_allowed_cpu(allowed: &str) -> Option<u32> {
    allowed
        .split(',')
        .next()?
        .split('-')
        .next()?
        .trim()
        .parse()
        .ok()
}

fn affinity_cpu_count(allowed: &str) -> usize {
    allowed
        .split(',')
        .filter_map(|range| {
            let (first, last) = range.split_once('-').unwrap_or((range, range));
            Some(
                last.trim()
                    .parse::<usize>()
                    .ok()?
                    .saturating_sub(first.trim().parse::<usize>().ok()?)
                    + 1,
            )
        })
        .sum()
}

fn profile(database_dir: &Path) -> Value {
    let status = read("/proc/self/status");
    let affinity = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .unwrap_or("")
        .trim();
    let cpuinfo = read("/proc/cpuinfo");
    let model = cpuinfo
        .lines()
        .find_map(|line| line.strip_prefix("model name"))
        .and_then(|value| value.split_once(':').map(|(_, model)| model.trim()))
        .unwrap_or("")
        .to_owned();
    let meminfo = read("/proc/meminfo");
    let memory = meminfo
        .lines()
        .find(|line| line.starts_with("MemTotal:"))
        .unwrap_or("")
        .to_owned();
    let available = meminfo
        .lines()
        .find(|line| line.starts_with("MemAvailable:"))
        .unwrap_or("")
        .to_owned();
    let revision = command("git", &["rev-parse", "HEAD"]);
    let dirty = !command("git", &["status", "--porcelain"]).is_empty();
    let storage_raw = command("df", &["-T", "-P", &database_dir.to_string_lossy()]);
    let storage_fields: Vec<_> = storage_raw
        .lines()
        .nth(1)
        .unwrap_or("")
        .split_whitespace()
        .collect();
    let storage = if storage_fields.len() >= 6 {
        format!(
            "type={} total_kib={} available_kib={}",
            storage_fields[1], storage_fields[2], storage_fields[4]
        )
    } else {
        "unavailable".into()
    };
    json!({
        "os_kernel":command("uname", &["-sr"]), "cpu_model":model,
        "host_logical_cores":cpuinfo.lines().filter(|line| line.starts_with("processor")).count(),
        "effective_cores":std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        "affinity_cores":affinity_cpu_count(affinity),
        "allowed_cpu_affinity":affinity, "selected_cpu":first_allowed_cpu(affinity),
        "cgroup_cpu_max":read("/sys/fs/cgroup/cpu.max"),
        "host_memory":memory,"available_memory":available,
        "cgroup_memory_max":read("/sys/fs/cgroup/memory.max"),
        "storage":storage,"rustc":command("rustc", &["--version"]),
        "revision":revision,"source_dirty":dirty
    })
}

fn atomic_json(path: &Path, value: &Value) -> io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temporary, path)
}

fn svg(points: &[Value], field: &str, title: &str) -> String {
    let max = points
        .iter()
        .filter_map(|p| p[field].as_u64())
        .max()
        .unwrap_or(1)
        .max(1);
    let mut out = format!("<figure><figcaption>{}</figcaption><svg viewBox=\"0 0 620 180\" role=\"img\" aria-label=\"{}\"><path d=\"M40 10 V150 H610\" fill=\"none\" stroke=\"#899\"/>", escape_html(title), escape_html(title));
    for (i, point) in points.iter().enumerate() {
        let x = 75 + (i as i32 * 500 / points.len().max(1) as i32);
        let y = 145 - (point[field].as_u64().unwrap_or(0) * 125 / max) as i32;
        let color = if point["status"] == "sustainable" {
            "#15704a"
        } else {
            "#ab3d3d"
        };
        out.push_str(&format!("<circle cx=\"{x}\" cy=\"{y}\" r=\"6\" fill=\"{color}\"/><text x=\"{x}\" y=\"165\" text-anchor=\"middle\" font-size=\"11\">{}</text>", point["tenants"].as_u64().unwrap_or(0)));
    }
    out.push_str("</svg></figure>");
    out
}

fn tick_quantiles(result: &Value) -> (u64, u64, u64) {
    let mut durations: Vec<u64> = result["fixture"]["points"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|point| point["phase"] == "measured")
        .filter_map(|point| point["duration_ms"].as_u64())
        .collect();
    durations.sort_unstable();
    let quantile = |numerator: usize| -> u64 {
        if durations.is_empty() {
            0
        } else {
            let position = (durations.len() * numerator)
                .div_ceil(100)
                .saturating_sub(1);
            durations[position.min(durations.len() - 1)]
        }
    };
    (quantile(50), quantile(95), quantile(99))
}

fn timeline(result: &Value) -> String {
    let Some(points) = result["fixture"]["points"].as_array() else {
        return String::new();
    };
    let max_height = points
        .iter()
        .filter_map(|p| p["highwater"].as_u64())
        .max()
        .unwrap_or(1)
        .max(1);
    let mut highwater = String::new();
    let mut slowest = String::new();
    for (i, point) in points.iter().enumerate() {
        let x = 40 + i * 540 / points.len().max(1);
        let high = 145 - point["highwater"].as_u64().unwrap_or(0) * 125 / max_height;
        let low = 145 - point["min_tenant_cursor"].as_u64().unwrap_or(0) * 125 / max_height;
        highwater.push_str(&format!("{x},{high} "));
        slowest.push_str(&format!("{x},{low} "));
    }
    format!("<svg viewBox=\"0 0 620 170\" role=\"img\" aria-label=\"Network high-water and slowest tenant over ticks\"><path d=\"M40 10 V150 H600\" fill=\"none\" stroke=\"#899\"/><polyline points=\"{highwater}\" fill=\"none\" stroke=\"#2358a4\" stroke-width=\"3\"/><polyline points=\"{slowest}\" fill=\"none\" stroke=\"#c3781c\" stroke-width=\"3\"/></svg>" )
}

fn report(
    output: &Path,
    driver: &str,
    hardware: &Value,
    results: &[Value],
    faults: &[Value],
) -> io::Result<()> {
    let mut page = String::from("<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Engine stress</title><style>body{font:16px/1.5 system-ui,sans-serif;max-width:1100px;margin:1rem auto;padding:0 1rem;color:#17212b}table{border-collapse:collapse;width:100%}th,td{border-bottom:1px solid #ccd3db;text-align:left;padding:.5rem}figure{display:inline-block;margin:1rem 0;width:49%}svg{width:100%}.bad{color:#a32}.good{color:#175c38}pre{white-space:pre-wrap;overflow-wrap:anywhere;background:#f3f5f7;padding:1rem}</style><h1>Hardware used for this run</h1><table>");
    for (key, label) in [
        ("os_kernel", "OS and kernel"),
        ("cpu_model", "CPU model"),
        ("host_logical_cores", "Host logical cores"),
        ("effective_cores", "Effective cores"),
        ("affinity_cores", "Affinity cores"),
        ("allowed_cpu_affinity", "Allowed affinity"),
        ("selected_cpu", "Pinned CPU"),
        ("cgroup_cpu_max", "CPU quota"),
        ("host_memory", "Host RAM"),
        ("available_memory", "Available RAM"),
        ("cgroup_memory_max", "Memory limit"),
        ("storage", "Test DB filesystem"),
        ("rustc", "Rust"),
        ("sqlite_version", "SQLite"),
        ("revision", "Engine revision"),
        ("source_dirty", "Dirty source"),
    ] {
        let value = hardware[key]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| hardware[key].to_string());
        page.push_str(&format!(
            "<tr><th>{}</th><td>{}</td></tr>",
            escape_html(label),
            escape_html(&value)
        ));
    }
    page.push_str(&format!(
        "</table><p><strong>Scanner driver:</strong> <code>{}</code></p>",
        escape_html(driver)
    ));
    page.push_str("<h2>Workload and progress</h2><p>One CPU; file-backed SQLite with real migrations, WAL and synchronous=NORMAL. Plain custody and the production scanner run against a deterministic scripted daemon while independent SQLite and HTTP status readers plus an admin writer issue background work. A separate connection holds a SQLite write lock for a fixed interval each tick. The capacity sweep uses immediate RPC responses; the fault points below add scheduled RPC delay and failures, and a longer blocked writer followed by a drain period. A blocked writer is a contention proxy, not a real disk fault. Capacity below is observational for this workload and machine.</p>");
    let highest = results
        .iter()
        .filter(|r| r["status"] == "sustainable")
        .filter_map(|r| r["tenants"].as_u64())
        .max();
    let first_overload = results
        .iter()
        .filter(|r| r["status"] == "overloaded")
        .filter_map(|r| r["tenants"].as_u64())
        .min();
    let bracket = match (highest, first_overload) {
        (Some(high), Some(fail)) => {
            format!("{high} tenants sustainable; first overload at {fail} tenants")
        }
        (Some(high), None) => format!("at least {high} tenants sustainable; no overload observed"),
        (None, Some(fail)) => {
            format!("no sustainable point observed; first overload at {fail} tenants")
        }
        (None, None) => "no capacity point completed".to_string(),
    };
    let readers = results
        .first()
        .and_then(|r| r["fixture"]["background_readers"].as_u64())
        .unwrap_or(0);
    let http_readers = results
        .first()
        .and_then(|r| r["fixture"]["background_http_readers"].as_u64())
        .unwrap_or(0);
    let writers = results
        .first()
        .and_then(|r| r["fixture"]["background_admin_writers"].as_u64())
        .unwrap_or(0);
    let lock_ms = results
        .first()
        .and_then(|r| r["fixture"]["write_lock_hold_ms_per_tick"].as_u64())
        .unwrap_or(0);
    let large = results
        .first()
        .and_then(|r| r["fixture"]["large_window_orders"].as_u64())
        .unwrap_or(0);
    page.push_str(&format!("<p><strong>Observed capacity bracket:</strong> {bracket}. Workload: two open orders per tenant plus one tenant with {large} open orders, one transaction per block, one new block per tick, {readers} database readers, {http_readers} HTTP status readers, {writers} admin writer, a {lock_ms} ms SQLite write lock per tick, 5 second tick budget, at most three blocks of oldest lag. Only this machine and schedule were tested.</p>"));
    page.push_str(&svg(
        results,
        "measured_duration_ms",
        "Measured tick time by tenant count (ms)",
    ));
    page.push_str(&svg(
        results,
        "final_lagging_tenants",
        "Lagging tenants after drain",
    ));
    page.push_str("<table><thead><tr><th>Tenants</th><th>Status</th><th>Measured time</th><th>Tick p50/p95/p99</th><th>Timer delay</th><th>DB reads/writes</th><th>HTTP reads</th><th>Max DB read/write latency</th><th>Max queue wait read/write</th><th>Max query read/write</th><th>Max HTTP latency</th><th>WAL pending pages</th><th>Lag after drain</th><th>Raw data</th></tr></thead><tbody>");
    for result in results {
        let n = result["tenants"].as_u64().unwrap_or(0);
        let status = result["status"].as_str().unwrap_or("incomplete");
        let (p50, p95, p99) = tick_quantiles(result);
        let pending_pages = result["fixture"]["wal_log_pages"]
            .as_i64()
            .unwrap_or(0)
            .saturating_sub(
                result["fixture"]["wal_checkpointed_pages"]
                    .as_i64()
                    .unwrap_or(0),
            );
        // Worker metrics exist only for engines with dedicated database
        // workers; older drivers leave them out rather than report zero.
        let pair = |read: &str, write: &str| -> String {
            let fixture = &result["fixture"];
            let show = |v: Option<u64>| v.map_or_else(|| "n/a".to_string(), |v| v.to_string());
            match (fixture[read].as_u64(), fixture[write].as_u64()) {
                (None, None) => "n/a".into(),
                (r, w) => format!("{}/{} µs", show(r), show(w)),
            }
        };
        page.push_str(&format!("<tr><td>{n}</td><td class=\"{}\">{}</td><td>{} ms</td><td>{p50}/{p95}/{p99} ms</td><td>{} µs</td><td>{}/{}</td><td>{}</td><td>{}/{} µs</td><td>{}</td><td>{}</td><td>{} µs</td><td>{pending_pages}</td><td>{}</td><td><a href=\"point-{n}.json\">JSON</a> · <a href=\"point-{n}.log\">log</a></td></tr>", if status == "sustainable" {"good"} else {"bad"}, escape_html(status), result["measured_duration_ms"].as_u64().unwrap_or(0), result["timer_max_delay_us"].as_u64().unwrap_or(0), result["fixture"]["background_reads_completed"].as_u64().unwrap_or(0), result["fixture"]["background_writes_completed"].as_u64().unwrap_or(0), result["fixture"]["background_http_reads_completed"].as_u64().unwrap_or(0), result["fixture"]["read_max_latency_us"].as_u64().unwrap_or(0), result["fixture"]["write_max_latency_us"].as_u64().unwrap_or(0), pair("db_read_queue_wait_max_us", "db_write_queue_wait_max_us"), pair("db_read_query_max_us", "db_write_query_max_us"), result["fixture"]["http_max_latency_us"].as_u64().unwrap_or(0), result["final_lagging_tenants"].as_u64().unwrap_or(0)));
    }
    page.push_str("</tbody></table><h2>Progress timelines</h2><p><span style=\"color:#2358a4\">Blue: network high-water</span>; <span style=\"color:#c3781c\">orange: slowest tenant cursor</span>. The gap is work still to catch up.</p>");
    for result in results {
        page.push_str(&format!(
            "<h3>{} tenants</h3>{}",
            result["tenants"].as_u64().unwrap_or(0),
            timeline(result)
        ));
    }
    page.push_str("<h2>Fault recovery</h2><table><tr><th>Fault</th><th>Result</th><th>Fault detail</th><th>Final slowest cursor</th><th>Raw data</th></tr>");
    for fault in faults {
        let name = fault["name"].as_str().unwrap_or("unknown");
        let status = fault["status"].as_str().unwrap_or("incomplete");
        let file = fault["file"].as_str().unwrap_or("fault-rpc");
        page.push_str(&format!("<tr><td>{}</td><td class=\"{}\">{}</td><td>{}</td><td>{}</td><td><a href=\"{}.json\">JSON</a> · <a href=\"{}.log\">log</a></td></tr>",
            escape_html(name), if status == "recovered" {"good"} else {"bad"},
            escape_html(status), escape_html(fault["detail"].as_str().unwrap_or("")),
            fault["min_tenant_cursor"].as_u64().unwrap_or(0), escape_html(file), escape_html(file)));
    }
    page.push_str("</table><p>The custody fault uses one scan slot with a scripted 5 ms service delay while retaining real plain-custody matching. Actual slow disk commands and process-kill recovery are not measured by this fixture. The RPC fault point uses deterministic transient failures and a drain period; the capacity sweep has no RPC delay.</p><p><a href=\"run.json\">Run metadata</a> · <a href=\"hardware.json\">Hardware JSON</a></p></html>");
    fs::write(output.join("index.html"), page)
}

/// The fixture binary, run pinned to `cpu` on Linux, with its output limited
/// to a bounded log and its JSON result parsed (`Null` when it produced none).
fn run_fixture(
    output: &Path,
    name: &str,
    cpu: Option<u64>,
    args: &[String],
) -> io::Result<(bool, Option<i32>, Value, String)> {
    // Where cargo builds it: `CARGO_TARGET_DIR` when set.
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .map(|dir| {
            if dir.is_absolute() {
                dir
            } else {
                root().join(dir)
            }
        })
        .unwrap_or_else(|| root().join("target"));
    let binary = target.join("debug/stress_fixture");
    if !binary.exists() {
        return Err(io::Error::other(format!(
            "{} does not exist: build it first (cargo build -p scanner --bin stress_fixture)",
            binary.display()
        )));
    }
    let mut cmd = if cfg!(target_os = "linux") {
        let mut cmd = Command::new("taskset");
        cmd.arg("-c").arg(cpu.unwrap_or(0).to_string()).arg(&binary);
        cmd
    } else {
        Command::new(&binary)
    };
    let run = cmd
        .args(args)
        .current_dir(root())
        .stdin(Stdio::null())
        .output()?;
    let log = String::from_utf8_lossy(&run.stderr);
    fs::write(
        output.join(format!("{name}.log")),
        &log.as_bytes()[..log.len().min(65_536)],
    )?;
    let fixture: Value = serde_json::from_slice(&run.stdout).unwrap_or(Value::Null);
    let command = format!(
        "taskset -c {} stress_fixture {}",
        cpu.unwrap_or(0),
        args.join(" ")
    );
    Ok((run.status.success(), run.status.code(), fixture, command))
}

/// The arguments every fixture run shares, from the scenario.
fn base_args(scenario: &Value, driver: &str, db: &Path, tenants: u64) -> Vec<String> {
    let mut args = vec![
        "--db".to_owned(),
        db.to_string_lossy().into_owned(),
        "--driver".into(),
        driver.into(),
        "--tenants".into(),
        tenants.to_string(),
    ];
    for (flag, key) in [
        ("--orders", "orders_per_tenant"),
        ("--large-window-orders", "large_window_orders"),
        ("--seed", "seed"),
        ("--background-readers", "background_readers"),
        ("--background-http-readers", "background_http_readers"),
        ("--background-writers", "background_admin_writers"),
    ] {
        args.push(flag.into());
        args.push(scenario[key].to_string());
    }
    args
}

fn fresh_db(output: &Path, name: &str) -> std::path::PathBuf {
    let db = output.join(format!("{name}.db"));
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{suffix}", db.display()));
    }
    db
}

/// `cargo xtask stress <ci|full|open> [driver]`. The default driver's report
/// goes to `target/coverage/stress`; another driver's to
/// `target/coverage/stress-<driver>`, so two engines can be compared.
pub fn run(profile_name: &str, driver: Option<&str>) -> io::Result<bool> {
    let output = match driver {
        None => root().join("target/coverage/stress"),
        Some(name) => root().join(format!("target/coverage/stress-{name}")),
    };
    let driver = driver.unwrap_or(DEFAULT_DRIVER);
    if profile_name == "open" {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        return Ok(Command::new(opener)
            .arg(output.join("index.html"))
            .status()?
            .success());
    }
    if profile_name != "ci" && profile_name != "full" {
        return Ok(false);
    }
    if output.exists() {
        fs::remove_dir_all(&output)?;
    }
    fs::create_dir_all(&output)?;
    let raw_scenario = fs::read(root().join("xtask/stress/scenario_v3.json"))?;
    let scenario: Value = serde_json::from_slice(&raw_scenario)?;
    let checksum = hex::encode(Sha256::digest(&raw_scenario));
    let built = Command::new("cargo")
        .args([
            "build",
            "-p",
            "scanner",
            "--bin",
            "stress_fixture",
            "--locked",
        ])
        .current_dir(root())
        .status()?;
    if !built.success() {
        return Err(io::Error::other("stress fixture build failed"));
    }
    let mut hardware = profile(&output);
    hardware["sqlite_version"] = json!(command(
        &root().join("target/debug/stress_fixture").to_string_lossy(),
        &["--version"]
    ));
    atomic_json(&output.join("hardware.json"), &hardware)?;
    let cpu = hardware["selected_cpu"].as_u64();
    if cfg!(target_os = "linux") && cpu.is_none() {
        return Err(io::Error::other("unable to find allowed CPU affinity"));
    }
    let points = scenario[if profile_name == "ci" {
        "ci_tenant_points"
    } else {
        "full_tenant_points"
    }]
    .as_array()
    .ok_or_else(|| io::Error::other("scenario has no tenant points"))?;
    let mut results = Vec::new();
    let mut faults: Vec<Value> = Vec::new();
    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let write_run = |results: &[Value], faults: &[Value]| {
        atomic_json(
            &output.join("run.json"),
            &json!({"profile":profile_name,"driver":driver,"started_at_utc":started_at,
                "scenario":scenario,"scenario_checksum":checksum,"hardware":"hardware.json","results":results,"faults":faults}),
        )
    };
    for point in points {
        let tenants = point
            .as_u64()
            .ok_or_else(|| io::Error::other("invalid tenant point"))?;
        let name = format!("point-{tenants}");
        let db = fresh_db(&output, &name);
        let mut args = base_args(&scenario, driver, &db, tenants);
        for (flag, key) in [
            ("--warmup", "warmup_ticks"),
            ("--measured", "measured_ticks"),
            ("--drain", "drain_ticks"),
            ("--write-lock-ms", "write_lock_hold_ms_per_tick"),
        ] {
            args.push(flag.into());
            args.push(scenario[key].to_string());
        }
        let (success, exit_code, fixture, command) = run_fixture(&output, &name, cpu, &args)?;
        let final_point = fixture["points"].as_array().and_then(|p| p.last());
        let final_lagging = final_point
            .and_then(|p| p["lagging_tenants"].as_u64())
            .unwrap_or(u64::MAX);
        let oldest_lag = final_point
            .and_then(|p| p["oldest_lag_blocks"].as_u64())
            .unwrap_or(u64::MAX);
        let min_cursor = final_point
            .and_then(|p| p["min_tenant_cursor"].as_u64())
            .unwrap_or(0);
        let progressed_tenants = final_point
            .and_then(|p| p["progressed_tenants"].as_u64())
            .unwrap_or(0);
        let measured_points: Vec<&Value> = fixture["points"]
            .as_array()
            .map(|points| points.iter().filter(|p| p["phase"] == "measured").collect())
            .unwrap_or_default();
        let backlog_not_growing = measured_points
            .first()
            .zip(measured_points.last())
            .is_some_and(|(first, last)| {
                last["lagging_tenants"].as_u64().unwrap_or(u64::MAX)
                    <= first["lagging_tenants"].as_u64().unwrap_or(0)
            });
        let within_tick_budget = measured_points.iter().all(|p| {
            p["duration_ms"].as_u64().unwrap_or(u64::MAX)
                <= scenario["poll_interval_ms"].as_u64().unwrap_or(0)
        });
        let tick_ok = fixture["points"]
            .as_array()
            .is_some_and(|p| p.iter().all(|row| row["ok"] == true));
        let background_progress = fixture["background_reads_completed"].as_u64().unwrap_or(0) > 0
            && fixture["background_writes_completed"].as_u64().unwrap_or(0) > 0
            && fixture["background_http_reads_completed"]
                .as_u64()
                .unwrap_or(0)
                > 0;
        let initial_min_cursor = fixture["points"]
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row["min_tenant_cursor"].as_u64())
            .unwrap_or(u64::MAX);
        let responsive = fixture["http_max_latency_us"].as_u64().unwrap_or(u64::MAX)
            <= scenario["max_http_latency_ms"].as_u64().unwrap_or(0) * 1000
            && fixture["timer_max_delay_us"].as_u64().unwrap_or(u64::MAX)
                <= scenario["max_timer_delay_ms"].as_u64().unwrap_or(0) * 1000;
        // Failed: the fixture broke, a tenant never moved, or the process
        // stopped answering. Overloaded: correct but too slow for the budget.
        let status = if !success
            || fixture["schema_version"] != scenario["schema_version"]
            || !tick_ok
            || min_cursor == 0
            || min_cursor <= initial_min_cursor
            || progressed_tenants != tenants
            || !background_progress
            || !responsive
        {
            "failed"
        } else if backlog_not_growing
            && within_tick_budget
            && oldest_lag <= scenario["max_oldest_lag_blocks"].as_u64().unwrap_or(0)
        {
            "sustainable"
        } else {
            "overloaded"
        };
        let result = json!({"tenants":tenants,"status":status,"exit_code":exit_code,
            "scenario_version":scenario["schema_version"],"scenario_checksum":checksum,"seed":scenario["seed"],
            "command":command,"hardware":"hardware.json","measured_duration_ms":fixture["measured_duration_ms"],
            "timer_max_delay_us":fixture["timer_max_delay_us"],"final_lagging_tenants":final_lagging,
            "final_oldest_lag_blocks":oldest_lag,"min_tenant_cursor":min_cursor,
            "progressed_tenants":progressed_tenants,"fixture":fixture});
        atomic_json(&output.join(format!("{name}.json")), &result)?;
        results.push(result);
        write_run(&results, &faults)?;
        report(&output, driver, &hardware, &results, &faults)?;
    }

    // Fault points: a short scheduled fault, then a drain in which every
    // tenant must recover.
    let fault_tenants = scenario["fault_tenants"].as_u64().unwrap_or(16);
    // (output file, title, extra fixture flags)
    type Flags = Vec<(&'static str, String)>;
    let fault_points: [(&str, &str, Flags); 3] = [
        (
            "fault-rpc",
            "transient daemon RPC failures under SQLite contention",
            vec![
                (
                    "--write-lock-ms",
                    scenario["write_lock_hold_ms_per_tick"].to_string(),
                ),
                ("--rpc-delay-ms", scenario["fault_rpc_delay_ms"].to_string()),
                (
                    "--rpc-fail-until-height",
                    scenario["fault_rpc_fail_until_height"].to_string(),
                ),
                (
                    "--rpc-fail-every",
                    scenario["fault_rpc_fail_every"].to_string(),
                ),
            ],
        ),
        (
            "fault-sqlite-lock",
            "1.5 s blocked SQLite writer, then recovery",
            vec![
                (
                    "--write-lock-ms",
                    scenario["fault_sqlite_lock_ms"].to_string(),
                ),
                ("--write-lock-until-tick", "2".into()),
            ],
        ),
        (
            "fault-custody",
            "single custody scan slot under concurrent tenant scans",
            vec![
                (
                    "--write-lock-ms",
                    scenario["write_lock_hold_ms_per_tick"].to_string(),
                ),
                (
                    "--custody-slots",
                    scenario["fault_custody_slots"].to_string(),
                ),
                (
                    "--custody-delay-ms",
                    scenario["fault_custody_delay_ms"].to_string(),
                ),
            ],
        ),
    ];
    for (file, title, extra) in fault_points {
        let db = fresh_db(&output, file);
        let mut args = base_args(&scenario, driver, &db, fault_tenants);
        for (flag, value) in [
            ("--warmup", "0".to_string()),
            ("--measured", "2".into()),
            ("--drain", scenario["fault_drain_ticks"].to_string()),
        ]
        .into_iter()
        .chain(extra)
        {
            args.push(flag.into());
            args.push(value);
        }
        let (success, _, fixture, command) = run_fixture(&output, file, cpu, &args)?;
        let points = fixture["points"].as_array();
        let min_cursor = points
            .and_then(|p| p.last())
            .and_then(|p| p["min_tenant_cursor"].as_u64())
            .unwrap_or(0);
        let common = success
            && fixture["schema_version"] == scenario["schema_version"]
            && min_cursor >= 2
            && points.is_some_and(|p| p.iter().rev().take(3).all(|row| row["ok"] == true))
            && fixture["background_http_reads_completed"]
                .as_u64()
                .unwrap_or(0)
                > 0;
        let (recovered, detail) = match file {
            "fault-rpc" => (
                common && fixture["rpc_failures"].as_u64().unwrap_or(0) > 0,
                format!(
                    "{} injected RPC failures",
                    fixture["rpc_failures"].as_u64().unwrap_or(0)
                ),
            ),
            "fault-sqlite-lock" => (
                common
                    && fixture["timer_max_delay_us"].as_u64().unwrap_or(u64::MAX)
                        <= scenario["max_timer_delay_ms"].as_u64().unwrap_or(0) * 1000,
                format!(
                    "{} ms per tick for two ticks",
                    scenario["fault_sqlite_lock_ms"]
                ),
            ),
            _ => (
                common
                    && fixture["custody_scans_completed"].as_u64().unwrap_or(0) > 0
                    && fixture["custody_max_wait_us"].as_u64().unwrap_or(0) > 1000,
                format!(
                    "{} scans; max slot wait {} µs",
                    fixture["custody_scans_completed"].as_u64().unwrap_or(0),
                    fixture["custody_max_wait_us"].as_u64().unwrap_or(0)
                ),
            ),
        };
        let fault = json!({"name":title,"file":file,"detail":detail,
            "status":if recovered {"recovered"} else {"failed"},
            "tenants":fault_tenants,"min_tenant_cursor":min_cursor,
            "scenario_version":scenario["schema_version"],"scenario_checksum":checksum,
            "command":command,"fixture":fixture});
        atomic_json(&output.join(format!("{file}.json")), &fault)?;
        faults.push(fault);
        write_run(&results, &faults)?;
        report(&output, driver, &hardware, &results, &faults)?;
    }
    Ok(faults.iter().all(|f| f["status"] == "recovered")
        && results.iter().all(|r| r["status"] != "failed"))
}
