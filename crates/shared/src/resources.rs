//! The machine a process runs on and the share of it the process may use
//! (docs/engine_scaling.md sections 3 and 6): the memory limit the scan
//! budget is checked against, and the identity that says whether two
//! processes share a machine.

use std::sync::OnceLock;

/// The machine's memory and, in a container, its cgroup limit, read once:
/// neither changes while a process runs.
struct Machine {
    memory_bytes: Option<u64>,
    cgroup_memory_bytes: Option<u64>,
}

fn machine() -> &'static Machine {
    static MACHINE: OnceLock<Machine> = OnceLock::new();
    MACHINE.get_or_init(|| {
        let mut system = sysinfo::System::new();
        system.refresh_memory();
        let memory_bytes = Some(system.total_memory()).filter(|bytes| *bytes > 0);
        let cgroup_memory_bytes = system
            .cgroup_limits()
            .map(|limits| limits.total_memory)
            .filter(|bytes| *bytes > 0);
        Machine {
            memory_bytes,
            cgroup_memory_bytes,
        }
    })
}

/// The machine's total memory, if it can be read.
pub fn machine_memory_bytes() -> Option<u64> {
    machine().memory_bytes
}

/// This process's container memory limit, if it runs under one.
pub fn cgroup_memory_bytes() -> Option<u64> {
    machine().cgroup_memory_bytes
}

/// The memory this process can use: the smaller of the machine's memory and
/// its container limit. `None` if neither can be read.
pub fn memory_limit_bytes() -> Option<u64> {
    smaller(machine_memory_bytes(), cgroup_memory_bytes())
}

fn smaller(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The machine's logical CPUs.
pub fn cpu_count() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

/// What names the machine (more exactly, the running kernel): the boot id,
/// which every container on one host shares, or else the host name and
/// memory. Two processes reporting the same one share a machine, so their
/// CPU and memory can be added up.
pub fn host_id() -> String {
    static HOST: OnceLock<String> = OnceLock::new();
    HOST.get_or_init(|| {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| {
                format!(
                    "{}:{}",
                    sysinfo::System::host_name().unwrap_or_default(),
                    machine_memory_bytes().unwrap_or(0)
                )
            })
    })
    .clone()
}

/// How often a process samples itself, and how long it keeps samples.
pub const SAMPLE_EVERY_SECS: i64 = 10;
const KEEP_SAMPLES: usize = 360;

/// One sample of a process: its share of the whole machine's CPU over the
/// last interval, and its resident memory. `unix` is the start of the
/// 10-second slot it falls in, so two processes' samples line up.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResourceSample {
    pub unix: i64,
    /// 0 to 100: a share of all the machine's cores together.
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

/// A process's last hour, with the machine it ran on.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResourceReport {
    /// See [`host_id`]: equal for two processes on one machine.
    pub host_id: String,
    pub cpu_count: usize,
    pub machine_memory_bytes: Option<u64>,
    /// The process's container memory limit, if it has one.
    pub cgroup_memory_bytes: Option<u64>,
    /// Oldest first, one per 10-second slot sampled.
    pub samples: Vec<ResourceSample>,
}

/// Samples its own process every [`SAMPLE_EVERY_SECS`], keeping an hour.
pub struct Sampler {
    state: parking_lot::Mutex<SamplerState>,
}

struct SamplerState {
    system: sysinfo::System,
    pid: Option<sysinfo::Pid>,
    samples: std::collections::VecDeque<ResourceSample>,
}

impl Default for Sampler {
    fn default() -> Self {
        Sampler {
            state: parking_lot::Mutex::new(SamplerState {
                system: sysinfo::System::new(),
                pid: sysinfo::get_current_pid().ok(),
                samples: std::collections::VecDeque::new(),
            }),
        }
    }
}

impl Sampler {
    /// Takes a sample now. The first one's CPU share is 0: CPU use is
    /// measured between two samples.
    pub fn sample(&self) {
        self.sample_at(crate::time::now_unix());
    }

    fn sample_at(&self, now_unix: i64) {
        let mut state = self.state.lock();
        let Some(pid) = state.pid else { return };
        state.system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[pid]),
            true,
            sysinfo::ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory(),
        );
        let Some(process) = state.system.process(pid) else {
            return;
        };
        let cores = cpu_count() as f32;
        let sample = ResourceSample {
            unix: now_unix - now_unix.rem_euclid(SAMPLE_EVERY_SECS),
            cpu_percent: (process.cpu_usage() / cores).clamp(0.0, 100.0),
            memory_bytes: process.memory(),
        };
        state.push(sample);
    }

    /// The last hour, with the machine.
    pub fn report(&self) -> ResourceReport {
        ResourceReport {
            host_id: host_id(),
            cpu_count: cpu_count(),
            machine_memory_bytes: machine_memory_bytes(),
            cgroup_memory_bytes: cgroup_memory_bytes(),
            samples: self.state.lock().samples.iter().copied().collect(),
        }
    }
}

impl SamplerState {
    /// Adds `sample`, replacing one already taken in its slot.
    fn push(&mut self, sample: ResourceSample) {
        if self
            .samples
            .back()
            .is_some_and(|last| last.unix == sample.unix)
        {
            self.samples.pop_back();
        }
        self.samples.push_back(sample);
        while self.samples.len() > KEEP_SAMPLES {
            self.samples.pop_front();
        }
    }
}

/// This process's sampler.
pub fn sampler() -> &'static Sampler {
    static SAMPLER: OnceLock<Sampler> = OnceLock::new();
    SAMPLER.get_or_init(Sampler::default)
}

/// Starts sampling this process every [`SAMPLE_EVERY_SECS`] on the current
/// Tokio runtime, for as long as it runs.
pub fn start_sampling() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {
        let mut every =
            tokio::time::interval(std::time::Duration::from_secs(SAMPLE_EVERY_SECS as u64));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            // `sysinfo` reads /proc: off the runtime's worker threads.
            let _ = tokio::task::spawn_blocking(|| sampler().sample()).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limit_is_the_smaller_of_the_machine_and_its_container() {
        assert_eq!(smaller(Some(8), Some(2)), Some(2));
        assert_eq!(smaller(Some(8), None), Some(8));
        assert_eq!(smaller(None, Some(2)), Some(2));
        assert_eq!(smaller(None, None), None);
    }

    #[test]
    fn a_process_samples_itself_into_10_second_slots_and_keeps_an_hour() {
        let sampler = Sampler::default();
        sampler.sample_at(1_000_003);
        sampler.sample_at(1_000_007);
        let report = sampler.report();
        assert_eq!(report.samples.len(), 1, "one per slot");
        assert_eq!(report.samples[0].unix, 1_000_000);
        assert!(report.samples[0].memory_bytes > 0);
        assert!((0.0..=100.0).contains(&report.samples[0].cpu_percent));
        assert_eq!(report.host_id, host_id());

        for slot in 1..=400 {
            sampler.sample_at(1_000_000 + slot * 10);
        }
        let samples = sampler.report().samples;
        assert_eq!(samples.len(), KEEP_SAMPLES);
        assert_eq!(samples.last().unwrap().unix, 1_004_000);
        assert!(samples.windows(2).all(|w| w[1].unix - w[0].unix == 10));
    }

    #[test]
    fn this_machine_can_be_read() {
        let memory = machine_memory_bytes().expect("the test machine's memory");
        assert!(memory > 64 * 1024 * 1024, "{memory}");
        assert!(memory_limit_bytes().unwrap() <= memory);
        assert!(cpu_count() >= 1);
        assert!(!host_id().is_empty());
        assert_eq!(host_id(), host_id(), "stable");
    }
}
