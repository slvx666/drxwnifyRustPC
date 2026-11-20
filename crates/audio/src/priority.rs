//! Thread priority and real-time scheduling utilities for audio anti-stutter.

#[cfg(windows)]
pub fn promote_thread_to_realtime_audio() {
    unsafe {
        // 1. Boost Windows thread priority to TIME_CRITICAL (15)
        unsafe extern "system" {
            fn GetCurrentThread() -> *mut std::ffi::c_void;
            fn SetThreadPriority(hThread: *mut std::ffi::c_void, nPriority: i32) -> i32;
            fn LoadLibraryA(lpLibFileName: *const u8) -> *mut std::ffi::c_void;
            fn GetProcAddress(hModule: *mut std::ffi::c_void, lpProcName: *const u8) -> *mut std::ffi::c_void;
        }
        const THREAD_PRIORITY_TIME_CRITICAL: i32 = 15;
        let thread = GetCurrentThread();
        let _ = SetThreadPriority(thread, THREAD_PRIORITY_TIME_CRITICAL);

        // 2. Register with Windows Multimedia Class Scheduler Service (MMCSS)
        let avrt = LoadLibraryA(b"avrt.dll\0".as_ptr());
        if !avrt.is_null() {
            let func = GetProcAddress(avrt, b"AvSetMmThreadCharacteristicsW\0".as_ptr());
            if !func.is_null() {
                type AvSetMmThreadCharacteristicsWFn =
                    unsafe extern "system" fn(*const u16, *mut u32) -> *mut std::ffi::c_void;
                let av_set: AvSetMmThreadCharacteristicsWFn = std::mem::transmute(func);
                let task_name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
                let mut task_idx = 0u32;
                let _ = av_set(task_name.as_ptr(), &mut task_idx);
            }
        }
    }
}

#[cfg(windows)]
pub fn promote_thread_to_high_priority() {
    unsafe {
        unsafe extern "system" {
            fn GetCurrentThread() -> *mut std::ffi::c_void;
            fn SetThreadPriority(hThread: *mut std::ffi::c_void, nPriority: i32) -> i32;
        }
        const THREAD_PRIORITY_HIGHEST: i32 = 2;
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
    }
}

#[cfg(not(windows))]
pub fn promote_thread_to_realtime_audio() {}

#[cfg(not(windows))]
pub fn promote_thread_to_high_priority() {}
