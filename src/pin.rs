//! Pin the current thread to one CPU, for repeatable timings.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_impossible_cpu_is_refused() {
        assert!(!pin_to_cpu(100_000));
    }
}
