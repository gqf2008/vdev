//! Process CPU time, so the report can separate "how much CPU does this cost"
//! from "how long did the wall clock take" (they differ once you start
//! measuring the real audio pipeline, where callbacks wait on buffers).

#[cfg(windows)]
pub fn cpu_seconds() -> f64 {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn GetProcessTimes(
            process: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }

    unsafe {
        let (mut creation, mut exit, mut kernel, mut user) =
            (FileTime::default(), FileTime::default(), FileTime::default(), FileTime::default());
        if GetProcessTimes(GetCurrentProcess(), &mut creation, &mut exit, &mut kernel, &mut user) == 0 {
            return 0.0;
        }
        let ticks = |t: FileTime| ((t.high as u64) << 32 | t.low as u64) as f64;
        // FILETIME is in 100 ns units
        (ticks(kernel) + ticks(user)) * 1e-7
    }
}

#[cfg(not(windows))]
pub fn cpu_seconds() -> f64 {
    // macOS / Linux: good enough for the D3-D4 harness, swap in clock_gettime
    // with CLOCK_PROCESS_CPUTIME_ID / task_info when we get there.
    0.0
}
