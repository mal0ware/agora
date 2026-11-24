//! Pin the current thread to one CPU, and read how busy the machine is, for
//! repeatable and honest timings.
//!
//! On Windows and Linux this asks the operating system directly. Anywhere
//! else it does nothing and says so.

/// Pin the calling thread to logical CPU `cpu` and raise its priority.
/// Returns true when the affinity was set.
#[cfg(windows)]
pub fn pin_to_cpu(cpu: usize) -> bool {
    extern "system" {
        fn GetCurrentThread() -> isize;
        fn SetThreadAffinityMask(thread: isize, mask: usize) -> usize;
        fn SetThreadPriority(thread: isize, priority: i32) -> i32;
    }
    const THREAD_PRIORITY_HIGHEST: i32 = 2;
    if cpu >= usize::BITS as usize {
        return false;
    }
    // SAFETY: both calls take the pseudo-handle of the calling thread and
    // plain integers; neither keeps a pointer.
    unsafe {
        let thread = GetCurrentThread();
        let ok = SetThreadAffinityMask(thread, 1usize << cpu) != 0;
        SetThreadPriority(thread, THREAD_PRIORITY_HIGHEST);
        ok
    }
}

/// Pin the calling thread to logical CPU `cpu`. Returns true when the
/// affinity was set.
#[cfg(target_os = "linux")]
pub fn pin_to_cpu(cpu: usize) -> bool {
    extern "C" {
        fn sched_setaffinity(pid: i32, size: usize, mask: *const u64) -> i32;
    }
    let mut mask = [0u64; 16];
    if cpu >= mask.len() * 64 {
        return false;
    }
    mask[cpu / 64] = 1u64 << (cpu % 64);
    // SAFETY: the mask outlives the call and its size is passed in bytes;
    // pid 0 means the calling thread.
    unsafe { sched_setaffinity(0, std::mem::size_of_val(&mask), mask.as_ptr()) == 0 }
}

/// Pinning is not implemented on this platform.
#[cfg(not(any(windows, target_os = "linux")))]
pub fn pin_to_cpu(_cpu: usize) -> bool {
    false
}

/// CPU time the whole machine has spent busy and in total since boot, summed
/// over every logical CPU, in arbitrary but equal units. `None` where this is
/// not implemented.
///
/// Two readings give the share of the machine that was busy between them,
/// which is how the benchmark reports what else was running.
#[cfg(windows)]
pub fn machine_cpu_times() -> Option<(u64, u64)> {
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    extern "system" {
        fn GetSystemTimes(idle: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32;
    }
    let (mut idle, mut kernel, mut user) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    // SAFETY: the three pointers are to live, writable FILETIME-shaped
    // structs, which is all the call needs.
    let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } != 0;
    if !ok {
        return None;
    }
    let ticks = |t: &FileTime| (u64::from(t.high) << 32) | u64::from(t.low);
    // Kernel time includes idle time.
    let total = ticks(&kernel) + ticks(&user);
    Some((total - ticks(&idle), total))
}

/// CPU time the whole machine has spent busy and in total since boot.
#[cfg(target_os = "linux")]
pub fn machine_cpu_times() -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let fields: Vec<u64> = stat
        .lines()
        .next()?
        .split_whitespace()
        .skip(1)
        .filter_map(|f| f.parse().ok())
        .collect();
    if fields.len() < 5 {
        return None;
    }
    let total: u64 = fields.iter().take(8).sum();
    // Fields four and five are idle and waiting on I/O.
    Some((total - fields[3] - fields[4], total))
}

/// Not implemented on this platform.
#[cfg(not(any(windows, target_os = "linux")))]
pub fn machine_cpu_times() -> Option<(u64, u64)> {
    None
}

/// How many logical CPUs' worth of work the machine did between two readings
/// of [`machine_cpu_times`], given its number of logical CPUs.
pub fn busy_cpus(before: (u64, u64), after: (u64, u64), cpus: usize) -> f64 {
    let total = after.1.saturating_sub(before.1);
    if total == 0 {
        return 0.0;
    }
    after.0.saturating_sub(before.0) as f64 / total as f64 * cpus as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_cpus_is_a_share_of_the_machine() {
        assert_eq!(busy_cpus((0, 0), (25, 100), 8), 2.0);
        assert_eq!(busy_cpus((5, 5), (5, 5), 8), 0.0);
        if let Some((busy, total)) = machine_cpu_times() {
            assert!(busy <= total && total > 0);
        }
    }

    #[test]
    fn an_impossible_cpu_is_refused() {
        assert!(!pin_to_cpu(100_000));
    }
}
