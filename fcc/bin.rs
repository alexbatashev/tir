fn main() {
    keep_heap();
    fcc::driver::compiler_main();
}

/// A compile is short and frees most of what it allocates in bursts. glibc
/// answers each burst by returning the pages and each large request by
/// mapping fresh ones, so the kernel zeroes the same memory over and over:
/// more than half the page faults of a CoreMark compile. Keep the heap.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn keep_heap() {
    // SAFETY: `mallopt` only sets allocator parameters, and nothing else runs
    // yet.
    unsafe {
        libc::mallopt(libc::M_TRIM_THRESHOLD, 512 << 20);
        libc::mallopt(libc::M_MMAP_THRESHOLD, 32 << 20);
        libc::mallopt(libc::M_TOP_PAD, 64 << 20);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn keep_heap() {}
