//! Lowers the process's scheduling priority, so a long scan of a large
//! file stays in the background rather than competing with interactive
//! work.
//!
//! The two implementations differ in how they treat failure, deliberately:
//! on Windows a failed `SetPriorityClass` panics, because it should not be
//! possible for a process to fail to lower its *own* priority and a
//! failure there means something is wrong with the assumptions; on Unix
//! `setpriority` can legitimately fail under a restrictive RLIMIT_NICE, so
//! its result is ignored and the scan simply runs at normal priority.

#[cfg(windows)]
pub fn set_cpu_prio() {
    use std::ffi::c_void;

    type HANDLE = *mut c_void;

    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> HANDLE;
        fn SetPriorityClass(
            hProcess: HANDLE,
            dwPriorityClass: u32,
        ) -> i32;
    }

    unsafe {
        let process = GetCurrentProcess();

        if SetPriorityClass(process, BELOW_NORMAL_PRIORITY_CLASS) == 0 {
            panic!("SetPriorityClass failed");
        }
    }
}

#[cfg(unix)]
pub fn set_cpu_prio() {
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 5);
    }
}
