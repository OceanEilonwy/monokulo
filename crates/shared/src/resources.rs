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
    fn this_machine_can_be_read() {
        let memory = machine_memory_bytes().expect("the test machine's memory");
        assert!(memory > 64 * 1024 * 1024, "{memory}");
        assert!(memory_limit_bytes().unwrap() <= memory);
        assert!(cpu_count() >= 1);
        assert!(!host_id().is_empty());
        assert_eq!(host_id(), host_id(), "stable");
    }
}
