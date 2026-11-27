//! Process and thread settings for a steady, short path, as Sunshine sets them (docs/windows.md, "Timing"), each a
//! guard that puts things back when the session ends.

use std::os::windows::io::AsRawSocket;

use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::NetworkManagement::QoS::{
    QOSAddSocketToFlow, QOSCloseHandle, QOSCreateHandle, QOSTrafficTypeAudioVideo,
    QOSTrafficTypeVoice, QOS_NON_ADAPTIVE_FLOW, QOS_VERSION,
};
use windows::Win32::Networking::WinSock::SOCKET;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Power::{
    SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, OpenInputDesktop, SetThreadDesktop, DESKTOP_ACCESS_FLAGS,
    DF_ALLOWOTHERACCOUNTHOOK,
};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateWaitableTimerExW,
    GetCurrentProcess, SetPriorityClass, SetWaitableTimer, WaitForSingleObject,
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, HIGH_PRIORITY_CLASS, INFINITE, NORMAL_PRIORITY_CLASS,
    TIMER_ALL_ACCESS,
};

use crate::pipeline::Clock;

/// QueryPerformanceCounter in microseconds: the clock DXGI stamps flips with (LastPresentTime) and WGC's
/// SystemRelativeTime is in, so capture times need no conversion between clocks.
pub struct Qpc {
    freq: u64,
}

impl Qpc {
    pub fn new() -> Self {
        let mut f = 0i64;
        // SAFETY: valid out-pointer; never fails on Windows XP and later.
        unsafe { QueryPerformanceFrequency(&mut f) }.ok();
        Qpc {
            freq: f.max(1) as u64,
        }
    }

    pub fn ticks_to_us(&self, ticks: i64) -> u64 {
        (ticks.max(0) as u128 * 1_000_000 / self.freq as u128) as u64
    }
}

impl Default for Qpc {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for Qpc {
    fn now_us(&self) -> u64 {
        let mut t = 0i64;
        // SAFETY: valid out-pointer.
        unsafe { QueryPerformanceCounter(&mut t) }.ok();
        self.ticks_to_us(t)
    }
}

/// For the session: 1 ms timer resolution, a high-priority process, the display and system kept awake. Sunshine
/// (src/platform/windows/misc.cpp:1216-1228, display_base.cpp:254-258) also asks NtSetTimerResolution for 0.5 ms
/// and raises the GPU scheduling class; those are undocumented and come after measuring.
pub struct SessionGuard;

impl SessionGuard {
    pub fn start() -> Self {
        // SAFETY: plain calls without pointers.
        unsafe {
            timeBeginPeriod(1);
            let _ = SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS);
            SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED);
        }
        SessionGuard
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        // SAFETY: as above.
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS);
            let _ = SetPriorityClass(GetCurrentProcess(), NORMAL_PRIORITY_CLASS);
            timeEndPeriod(1);
        }
    }
}

/// MMCSS for the calling thread: "Capture" for the video thread, "Pro Audio" for WASAPI (Sunshine audio.cpp:701).
pub struct Mmcss(HANDLE);

impl Mmcss {
    pub fn join(task: windows::core::PCWSTR) -> Option<Self> {
        let mut index = 0u32;
        // SAFETY: task is a valid wide string, index a valid out-pointer.
        unsafe { AvSetMmThreadCharacteristicsW(task, &mut index) }
            .ok()
            .map(Mmcss)
    }

    pub fn capture() -> Option<Self> {
        Self::join(w!("Capture"))
    }

    pub fn pro_audio() -> Option<Self> {
        Self::join(w!("Pro Audio"))
    }
}

impl Drop for Mmcss {
    fn drop(&mut self) {
        // SAFETY: the handle came from AvSetMmThreadCharacteristicsW.
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.0) };
    }
}

/// Moves the calling thread to the desktop that takes input now: the user's, the secure desktop of a UAC prompt or
/// the lock screen. Desktop Duplication only sees the desktop of its thread, so this goes before every
/// DuplicateOutput (Sunshine misc.cpp:247-263). Reaching the secure desktop needs SYSTEM (a service), see
/// docs/windows.md, "Privileges".
pub fn follow_input_desktop() -> bool {
    // SAFETY: the desktop handle is closed after use; SetThreadDesktop keeps its own reference.
    unsafe {
        let Ok(d) = OpenInputDesktop(
            DF_ALLOWOTHERACCOUNTHOOK,
            false,
            DESKTOP_ACCESS_FLAGS(0x10000000),
        ) else {
            return false;
        };
        let ok = SetThreadDesktop(d).is_ok();
        let _ = CloseDesktop(d);
        ok
    }
}

/// A sleep to a deadline with the high-resolution waitable timer (Windows 10 1809+, Sunshine misc.cpp:1827-1833),
/// or a plain one before that.
pub struct Timer(HANDLE);

impl Timer {
    pub fn new() -> windows::core::Result<Self> {
        // SAFETY: no pointers but the optional name, None.
        unsafe {
            CreateWaitableTimerExW(
                None,
                None,
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
            .or_else(|_| CreateWaitableTimerExW(None, None, 0, TIMER_ALL_ACCESS.0))
            .map(Timer)
        }
    }

    pub fn sleep(&self, d: std::time::Duration) {
        // Negative: relative, in 100 ns.
        let due = -((d.as_nanos() / 100) as i64).max(1);
        // SAFETY: the timer handle is ours; due is a valid pointer for the call.
        unsafe {
            if SetWaitableTimer(self.0, &due, 0, None, None, false).is_ok() {
                WaitForSingleObject(self.0, INFINITE);
            }
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        // SAFETY: our handle.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// DSCP marks through qWAVE: Windows drops IP_TOS from applications, but a QoS flow sets CS5 for video and CS6/7 for
/// voice, which Wi-Fi maps to its video and voice queues (Sunshine misc.cpp:1683-1777). qWAVE is missing on
/// Windows Server; then nothing is marked.
pub struct Qos(HANDLE);

impl Qos {
    pub fn new() -> Option<Self> {
        let v = QOS_VERSION {
            MajorVersion: 1,
            MinorVersion: 0,
        };
        let mut h = HANDLE::default();
        // SAFETY: valid pointers for the call.
        unsafe { QOSCreateHandle(&v, &mut h) }
            .as_bool()
            .then_some(())?;
        Some(Qos(h))
    }

    /// Marks a connected UDP socket; `voice` for audio.
    pub fn add(&self, sock: &std::net::UdpSocket, voice: bool) -> bool {
        let kind = if voice {
            QOSTrafficTypeVoice
        } else {
            QOSTrafficTypeAudioVideo
        };
        let mut flow = 0u32;
        // SAFETY: the socket is connected (the destination is taken from it) and outlives the flow.
        unsafe {
            QOSAddSocketToFlow(
                self.0,
                SOCKET(sock.as_raw_socket() as usize),
                None,
                kind,
                Some(QOS_NON_ADAPTIVE_FLOW),
                &mut flow,
            )
        }
        .as_bool()
    }
}

impl Drop for Qos {
    fn drop(&mut self) {
        // SAFETY: our handle.
        let _ = unsafe { QOSCloseHandle(self.0) };
    }
}
