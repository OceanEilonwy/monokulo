//! The engine's own threads (`docs/engine_as_library.md` §5).
//!
//! A Tokio runtime with `server.worker_threads` workers, every thread of it
//! (workers and the blocking pool where scans run) named `engine-worker`,
//! kept to the CPUs `server.cpus` lists and run at `server.nice`.
//!
//! The same runtime serves the standalone engine and the engine inside
//! monokulo, so on a router the engine can be kept off the CPUs routing
//! needs (`server.cpus = "2,3"`) and below everything else
//! (`server.nice = 10`) without `taskset` or `nice` around the process: in
//! monokulo, only the engine's threads are pinned and niced, not
//! monokulo's.
//!
//! Pinning and niceness are per thread on Linux, which is where they are
//! supported; elsewhere, asking for either stops the engine at start rather
//! than being ignored.

use std::io;

/// What the engine's runtime is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadPlan {
    /// Worker threads (`server.worker_threads`).
    pub workers: usize,
    /// The CPUs every engine thread may run on; empty for all of them
    /// (`server.cpus`).
    pub cpus: Vec<usize>,
    /// The niceness of every engine thread, 0 (normal) to 19 (lowest)
    /// (`server.nice`).
    pub nice: i32,
}

/// The name of every thread of the engine's runtime.
pub const THREAD_NAME: &str = "engine-worker";

/// What every engine thread's name starts with.
///
/// That is its runtime's ([`THREAD_NAME`]), its database worker and readers
/// (`engine-db…`) and its `RandomX` hashers (`engine-randomx-…`). Inside
/// monokulo, the engine's log lines (`telemetry::Telemetry::host`) and its
/// share of the CPU (`shared::resources::Sampler::host_threads`) are told
/// apart by it.
pub const THREAD_PREFIX: &str = "engine-";

/// A CPU list as `taskset -c` takes it: numbers and ranges, comma
/// separated (`2,3`, `0-1,4`). Empty is every CPU.
pub fn parse_cpu_list(text: &str) -> Result<Vec<usize>, String> {
    let mut cpus = std::collections::BTreeSet::new();
    for part in text
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let number = |n: &str| {
            n.trim()
                .parse::<usize>()
                .map_err(|e| format!("{part:?} isn't a CPU number or a range such as 2-3 ({e})."))
        };
        if let Some((first, last)) = part.split_once('-') {
            let (first, last) = (number(first)?, number(last)?);
            if first > last {
                return Err(format!(
                    "{part:?} runs backwards: write the lower CPU first."
                ));
            }
            if last - first >= 1024 {
                return Err("At most 1024 CPUs.".to_owned());
            }
            for cpu in first..=last {
                cpus.insert(cpu);
                if cpus.len() > 1024 {
                    return Err("At most 1024 CPUs.".to_owned());
                }
            }
        } else {
            cpus.insert(number(part)?);
            if cpus.len() > 1024 {
                return Err("At most 1024 CPUs.".to_owned());
            }
        }
    }
    Ok(cpus.into_iter().collect())
}

impl ThreadPlan {
    /// How many scans may run at once: one per CPU the engine may use.
    pub fn scan_slots(&self) -> usize {
        if self.cpus.is_empty() {
            std::thread::available_parallelism().map_or(2, std::num::NonZeroUsize::get)
        } else {
            self.cpus.len()
        }
    }

    /// Whether the plan can be applied here: tried on a thread of its own,
    /// so CPUs that don't exist, or a niceness this process may not take,
    /// stop the engine at start, not each of its threads quietly.
    pub fn check(&self) -> Result<(), String> {
        if self.cpus.is_empty() && self.nice == 0 {
            return Ok(());
        }
        let plan = self.clone();
        std::thread::Builder::new()
            .name(format!("{THREAD_NAME}-check"))
            .spawn(move || plan.apply_to_this_thread())
            .map_err(|e| e.to_string())?
            .join()
            .map_err(|panic| format!("checking server.cpus and server.nice failed: {panic:?}"))?
            .map_err(|e| e.to_string())
    }

    /// The engine's runtime: its workers and blocking pool named
    /// [`THREAD_NAME`], each pinned and niced as it starts. Call
    /// [`Self::check`] first: a thread that can't take the plan logs why
    /// and runs as it is.
    pub fn build_runtime(&self) -> io::Result<tokio::runtime::Runtime> {
        let plan = self.clone();
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(self.workers)
            .thread_name(THREAD_NAME)
            .on_thread_start(move || {
                if let Err(e) = plan.apply_to_this_thread() {
                    tracing::error!(error = %e, "an engine thread couldn't take server.cpus or server.nice");
                }
            })
            .enable_all()
            .build()
    }

    /// Pins the calling thread to [`Self::cpus`] and sets its niceness.
    #[cfg(target_os = "linux")]
    fn apply_to_this_thread(&self) -> io::Result<()> {
        use rustix::thread::{gettid, sched_setaffinity, CpuSet};
        if !self.cpus.is_empty() {
            let mut set = CpuSet::new();
            for &cpu in &self.cpus {
                if cpu >= CpuSet::MAX_CPU {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "CPU {cpu} is beyond the {} this system can name",
                            CpuSet::MAX_CPU
                        ),
                    ));
                }
                set.set(cpu);
            }
            // `None` is the calling thread: affinity is per thread on Linux.
            sched_setaffinity(None, &set).map_err(|e| {
                let e = io::Error::from(e);
                io::Error::new(
                    e.kind(),
                    format!(
                        "server.cpus {:?}: {e} (are those CPUs on this machine?)",
                        self.cpus
                    ),
                )
            })?;
        }
        if self.nice != 0 {
            // With a thread id, PRIO_PROCESS sets that one thread's niceness
            // on Linux, not the whole process's.
            rustix::process::setpriority_process(Some(gettid()), self.nice).map_err(|e| {
                let e = io::Error::from(e);
                io::Error::new(e.kind(), format!("server.nice {}: {e}", self.nice))
            })?;
        }
        Ok(())
    }

    /// Pinning and niceness are Linux's here: asked for elsewhere, the
    /// engine doesn't start.
    #[cfg(not(target_os = "linux"))]
    fn apply_to_this_thread(&self) -> io::Result<()> {
        if self.cpus.is_empty() && self.nice == 0 {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "server.cpus and server.nice are only supported on Linux",
            ))
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn a_cpu_list_takes_numbers_and_ranges_like_taskset() {
        assert_eq!(parse_cpu_list(""), Ok(vec![]));
        assert_eq!(parse_cpu_list("2,3"), Ok(vec![2, 3]));
        assert_eq!(parse_cpu_list(" 0-1, 4 ,1"), Ok(vec![0, 1, 4]));
        assert!(parse_cpu_list("3-1").unwrap_err().contains("backwards"));
        assert!(parse_cpu_list("two").unwrap_err().contains("CPU number"));
        assert!(parse_cpu_list("0-5000").unwrap_err().contains("1024"));
    }

    #[test]
    fn scans_get_one_slot_per_cpu_the_engine_may_use() {
        let plan = ThreadPlan {
            workers: 2,
            cpus: vec![2, 3],
            nice: 0,
        };
        assert_eq!(plan.scan_slots(), 2);
        let all = ThreadPlan {
            cpus: vec![],
            ..plan
        };
        assert_eq!(
            all.scan_slots(),
            std::thread::available_parallelism().unwrap().get()
        );
    }

    /// The thread's own entry in /proc: its allowed CPUs and its niceness.
    #[cfg(target_os = "linux")]
    fn this_thread() -> (String, i32) {
        let status = std::fs::read_to_string("/proc/thread-self/status").unwrap();
        let cpus = status
            .lines()
            .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
            .unwrap()
            .trim()
            .to_owned();
        let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap();
        // Fields after the command name, which is in parentheses.
        let after = &stat[stat.rfind(')').unwrap() + 2..];
        let nice = after.split(' ').nth(16).unwrap().parse().unwrap();
        (cpus, nice)
    }

    /// Every engine thread, worker and blocking alike, runs on the CPUs and
    /// at the niceness asked for, under the engine's thread name; the thread
    /// that built the runtime is left as it was.
    #[cfg(target_os = "linux")]
    #[test]
    fn every_engine_thread_is_pinned_and_niced() {
        let before = this_thread();
        let plan = ThreadPlan {
            workers: 2,
            cpus: vec![0],
            nice: 7,
        };
        plan.check().unwrap();
        let runtime = plan.build_runtime().unwrap();
        let threads = runtime.block_on(async {
            let worker = tokio::spawn(async {
                (
                    std::thread::current().name().map(str::to_owned),
                    this_thread(),
                )
            })
            .await
            .unwrap();
            let blocking = tokio::task::spawn_blocking(|| {
                (
                    std::thread::current().name().map(str::to_owned),
                    this_thread(),
                )
            })
            .await
            .unwrap();
            [worker, blocking]
        });
        for (name, (cpus, nice)) in threads {
            assert_eq!(name.as_deref(), Some(THREAD_NAME));
            assert_eq!(cpus, "0");
            assert_eq!(nice, 7);
        }
        runtime.shutdown_background();
        assert_eq!(this_thread(), before, "the caller's thread is left alone");
    }

    /// A CPU the machine doesn't have stops the engine at start.
    #[cfg(target_os = "linux")]
    #[test]
    fn cpus_that_dont_exist_are_refused_before_the_runtime_starts() {
        let plan = ThreadPlan {
            workers: 1,
            cpus: vec![1000],
            nice: 0,
        };
        let refused = plan.check().unwrap_err();
        assert!(refused.contains("server.cpus [1000]"), "{refused}");
    }
}
