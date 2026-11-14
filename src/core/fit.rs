//! Whether a batch fits in the machine's memory: instances times their memory against the RAM that is free.

/// Memory of the machine in MB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Memory {
    pub total_mb: u64,
    pub free_mb: u64,
}

/// What `count` instances of `memory_mb` each need, what is free, and how many fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fit {
    pub needed_mb: u64,
    pub free_mb: u64,
    /// `count` when everything fits, otherwise as many as fit.
    pub suggested_count: u32,
}

impl Fit {
    /// Whether all instances fit.
    pub fn fits(&self) -> bool {
        self.needed_mb <= self.free_mb
    }
}

/// How `count` instances of `memory_mb` each sit in the memory free right now. `None` when the machine will not tell. Reads the system: call it off the UI thread.
pub fn fit(count: u32, memory_mb: u32) -> Option<Fit> {
    Some(fit_in(count, memory_mb, system_memory()?))
}

/// Same as [`fit`] for a known `memory`.
pub fn fit_in(count: u32, memory_mb: u32, memory: Memory) -> Fit {
    let each = u64::from(memory_mb.max(1));
    let needed_mb = u64::from(count) * each;
    let suggested_count = u32::try_from(memory.free_mb / each).unwrap_or(u32::MAX).min(count);
    Fit { needed_mb, free_mb: memory.free_mb, suggested_count }
}

/// Total and available RAM of this machine.
pub fn system_memory() -> Option<Memory> {
    platform::read()
}

#[cfg(any(target_os = "linux", test))]
fn parse_meminfo(text: &str) -> Option<Memory> {
    let kb = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)?.trim().strip_suffix("kB")?.trim().parse::<u64>().ok());
    let total = kb("MemTotal:")?;
    let free = kb("MemAvailable:").or_else(|| Some(kb("MemFree:")? + kb("Cached:").unwrap_or(0)))?;
    Some(Memory { total_mb: total / 1024, free_mb: free / 1024 })
}

#[cfg(any(target_os = "macos", test))]
fn parse_vm_stat(text: &str, total_bytes: u64) -> Option<Memory> {
    let page: u64 = text.lines().next()?.split("page size of ").nth(1)?.split_whitespace().next()?.parse().ok()?;
    let pages = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)?.trim().trim_end_matches('.').parse::<u64>().ok());
    let free = pages("Pages free:")? + pages("Pages inactive:").unwrap_or(0) + pages("Pages speculative:").unwrap_or(0) + pages("Pages purgeable:").unwrap_or(0);
    Some(Memory { total_mb: total_bytes >> 20, free_mb: (free * page) >> 20 })
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{Memory, parse_meminfo};

    pub fn read() -> Option<Memory> {
        parse_meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::process::Command;

    use super::{Memory, parse_vm_stat};

    fn run(program: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(program).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn read() -> Option<Memory> {
        let total: u64 = run("sysctl", &["-n", "hw.memsize"])?.trim().parse().ok()?;
        parse_vm_stat(&run("vm_stat", &[])?, total)
    }
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    use super::Memory;

    pub fn read() -> Option<Memory> {
        let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        status.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
        if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
            return None;
        }
        Some(Memory { total_mb: status.ullTotalPhys >> 20, free_mb: status.ullAvailPhys >> 20 })
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod platform {
    use super::Memory;

    pub fn read() -> Option<Memory> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEM: Memory = Memory { total_mb: 32768, free_mb: 32000 };

    #[test]
    fn everything_fits_when_free_memory_is_enough() {
        let f = fit_in(10, 2048, MEM);
        assert_eq!(f, Fit { needed_mb: 20480, free_mb: 32000, suggested_count: 10 });
        assert!(f.fits());
    }

    #[test]
    fn too_many_instances_get_a_smaller_suggestion() {
        let f = fit_in(50, 2048, MEM);
        assert_eq!((f.needed_mb, f.suggested_count), (102_400, 15));
        assert!(!f.fits());
    }

    #[test]
    fn no_free_memory_suggests_none_and_zero_memory_does_not_divide_by_zero() {
        assert_eq!(fit_in(5, 2048, Memory { total_mb: 8192, free_mb: 100 }).suggested_count, 0);
        assert_eq!(fit_in(5, 0, MEM).suggested_count, 5);
        assert_eq!(fit_in(0, 2048, MEM), Fit { needed_mb: 0, free_mb: 32000, suggested_count: 0 });
        assert_eq!(fit_in(u32::MAX, u32::MAX, MEM).needed_mb, u64::from(u32::MAX) * u64::from(u32::MAX));
    }

    #[test]
    fn proc_meminfo_is_read() {
        let text = "MemTotal:       16384000 kB\nMemFree:          500000 kB\nMemAvailable:    8192000 kB\nCached:  1000 kB\n";
        assert_eq!(parse_meminfo(text), Some(Memory { total_mb: 16000, free_mb: 8000 }));
        let old = "MemTotal: 2048000 kB\nMemFree: 1024000 kB\nCached: 1024000 kB\n";
        assert_eq!(parse_meminfo(old), Some(Memory { total_mb: 2000, free_mb: 2000 }));
        assert_eq!(parse_meminfo("nothing"), None);
    }

    #[test]
    fn vm_stat_is_read() {
        let text = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:                               1000.\nPages active:                            5000.\nPages inactive:                          3000.\nPages speculative:                        500.\nPages purgeable:                          100.\n";
        let m = parse_vm_stat(text, 16 << 30).unwrap();
        assert_eq!(m.total_mb, 16384);
        assert_eq!(m.free_mb, (4600 * 16384) >> 20);
        assert_eq!(parse_vm_stat("garbage", 1), None);
    }

    #[test]
    fn this_machine_reports_memory() {
        let m = system_memory().unwrap();
        assert!(m.total_mb > 0 && m.free_mb <= m.total_mb);
    }
}
