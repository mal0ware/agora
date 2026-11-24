//! A cache prefetch hint.

/// Ask the CPU to start loading the cache line at `p`. This is only a hint:
/// it reads nothing, cannot fault, and does nothing on other architectures.
#[inline(always)]
pub(crate) fn prefetch<T>(p: *const T) {
    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};
        // SAFETY: PREFETCHT0 takes any address, valid or not, and never
        // faults or changes program state.
        #[allow(unused_unsafe)]
        unsafe {
            _mm_prefetch::<_MM_HINT_T0>(p.cast::<i8>());
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = p;
    }
}
