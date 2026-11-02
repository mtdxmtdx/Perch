//! 桌面感知的廉价信号：前台是否在「看视频」、是否在开会/通话。
//!
//! 第一期只提供两个信号，供 [`crate::proactive`] 的桌面状态与打扰规则使用。
//! Windows 下读 Core Audio 峰值与麦克风 ConsentStore；纯判断逻辑（滞回、路径还原、
//! 条目是否仍有效）不依赖平台，方便单测。

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 出声峰值下限：低于此值视为无声（瞬时峰值表）
const PEAK_THRESHOLD: f32 = 0.01;
/// 信号缓存时长：主动聊天与 TTS 共用
const CACHE_TTL: Duration = Duration::from_secs(5);

/// 「在看」滞回状态：进入后前台需持续无声满 hold 才退出
#[derive(Clone, Debug, Default)]
pub struct WatchingHold {
    pub watching: bool,
    /// 进入「在看」后，前台开始无声的时刻；有声时清掉
    pub quiet_since: Option<Instant>,
}

/// 根据前台是否出声、用户空闲秒数，更新「在看」滞回并返回当前是否在看。
///
/// - 进入：尚未在看，且前台出声，且 `user_idle_secs >= idle_threshold`
/// - 退出：已在看，且前台持续无声满 `hold_secs`（对白间隙不会立刻退出）
pub fn update_watching(
    state: &mut WatchingHold,
    fg_playing: bool,
    user_idle_secs: u64,
    idle_threshold: u64,
    hold_secs: u64,
    now: Instant,
) -> bool {
    if state.watching {
        if fg_playing {
            state.quiet_since = None;
        } else {
            let since = *state.quiet_since.get_or_insert(now);
            if now.duration_since(since) >= Duration::from_secs(hold_secs) {
                state.watching = false;
                state.quiet_since = None;
            }
        }
    } else if fg_playing && user_idle_secs >= idle_threshold {
        state.watching = true;
        state.quiet_since = None;
    }
    state.watching
}

/// 麦克风 ConsentStore 里的一项（打包应用或 NonPackaged）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicConsentEntry {
    /// 子键名：包家族名，或 NonPackaged 下把 `\` 换成 `#` 的 exe 路径
    pub key_name: String,
    /// `LastUsedTimeStop`；为 0 表示仍在占用
    pub last_used_stop: i64,
    pub non_packaged: bool,
}

/// NonPackaged 子键名 → 真实 exe 路径（`#` → `\`）
pub fn decode_nonpackaged_key(key: &str) -> String {
    key.replace('#', "\\")
}

/// 该麦克风占用项现在是否算「通话中」：`LastUsedTimeStop == 0`，对应进程仍在跑，且不是 Perch 自己。
///
/// - NonPackaged：还原路径后与 `running_exe_paths` 做大小写不敏感比对；`self_exe` 为当前进程完整路径，命中则排除
/// - 打包应用：`key_name` 与 `running_package_families` 比对
pub fn mic_entry_active(
    entry: &MicConsentEntry,
    running_exe_paths: &[String],
    running_package_families: &[String],
    self_exe: &str,
) -> bool {
    if entry.last_used_stop != 0 {
        return false;
    }
    if entry.non_packaged {
        let path = decode_nonpackaged_key(&entry.key_name);
        if eq_path(&path, self_exe) {
            return false;
        }
        return running_exe_paths.iter().any(|p| eq_path(p, &path));
    }
    let family = entry.key_name.trim();
    if family.is_empty() {
        return false;
    }
    running_package_families.iter().any(|f| f.eq_ignore_ascii_case(family))
}

fn eq_path(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim().trim_matches('"').replace('/', "\\").to_lowercase();
    norm(a) == norm(b)
}

/// 前台进程名是否在「正在出声」的进程名集合里（不带 .exe，大小写不敏感）
pub fn foreground_is_playing(fg: Option<&str>, playing_names: &[String]) -> bool {
    let Some(fg) = fg.map(|s| s.trim().trim_end_matches(".exe")).filter(|s| !s.is_empty()) else {
        return false;
    };
    playing_names.iter().any(|p| {
        let p = p.trim().trim_end_matches(".exe");
        p.eq_ignore_ascii_case(fg)
    })
}

/// 一次采样得到的原始信号（未做「在看」滞回）
#[derive(Clone, Debug, Default)]
struct RawSignals {
    /// 正在出声的进程名（不带 .exe，小写）
    playing_processes: Vec<String>,
    in_call: bool,
}

#[derive(Default)]
struct Cache {
    at: Option<Instant>,
    raw: RawSignals,
    hold: WatchingHold,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache {
    at: None,
    raw: RawSignals { playing_processes: Vec::new(), in_call: false },
    hold: WatchingHold { watching: false, quiet_since: None },
});

/// 供主动聊天 / TTS 使用的信号快照
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Signals {
    pub watching: bool,
    pub in_call: bool,
}

/// 读取（带 5 秒缓存）在看 / 通话信号。`desktop_modes` 关闭时调用方应忽略结果。
pub fn signals(fg: Option<&str>, user_idle_secs: u64, watching_idle_secs: u64, watching_hold_secs: u64) -> Signals {
    let now = Instant::now();
    let mut cache = CACHE.lock().unwrap();
    let stale = cache.at.is_none_or(|t| now.duration_since(t) >= CACHE_TTL);
    if stale {
        cache.raw = sample_raw();
        cache.at = Some(now);
    }
    let fg_playing = foreground_is_playing(fg, &cache.raw.playing_processes);
    let watching = update_watching(
        &mut cache.hold,
        fg_playing,
        user_idle_secs,
        watching_idle_secs,
        watching_hold_secs,
        now,
    );
    Signals { watching, in_call: cache.raw.in_call }
}

fn sample_raw() -> RawSignals {
    #[cfg(windows)]
    {
        sample_raw_windows()
    }
    #[cfg(not(windows))]
    {
        RawSignals::default()
    }
}

#[cfg(windows)]
fn sample_raw_windows() -> RawSignals {
    // Core Audio 要 COM：在专用线程里 CoInitializeEx(MTA) 后再查
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::spawn(move || {
        let playing = playing_processes_com().unwrap_or_default();
        let in_call = mic_in_use_windows().unwrap_or(false);
        let _ = tx.send(RawSignals { playing_processes: playing, in_call });
    })
    .join();
    rx.recv().unwrap_or_default()
}

#[cfg(windows)]
fn playing_processes_com() -> Option<Vec<String>> {
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let result = playing_processes_inner();
    unsafe {
        CoUninitialize();
    }
    result
}

#[cfg(windows)]
fn playing_processes_inner() -> Option<Vec<String>> {
    use windows::core::Interface;
    use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
    use windows::Win32::Media::Audio::{
        eMultimedia, eRender, IAudioSessionControl, IAudioSessionControl2, IAudioSessionEnumerator,
        IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.ok()?;
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) }.ok()?;
    let manager: IAudioSessionManager2 = unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) }.ok()?;
    let sessions: IAudioSessionEnumerator = unsafe { manager.GetSessionEnumerator() }.ok()?;
    let count = unsafe { sessions.GetCount() }.ok()? as i32;

    let self_pid = std::process::id();
    let mut names = Vec::new();
    for i in 0..count {
        let Ok(control) = (unsafe { sessions.GetSession(i) }) else { continue };
        let control: IAudioSessionControl = control;
        let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
        let Ok(pid) = (unsafe { control2.GetProcessId() }) else { continue };
        if pid == 0 || pid == self_pid {
            continue;
        }
        let Ok(meter) = control.cast::<IAudioMeterInformation>() else { continue };
        let Ok(peak) = (unsafe { meter.GetPeakValue() }) else { continue };
        if peak <= PEAK_THRESHOLD {
            continue;
        }
        if let Some(name) = crate::pet::win::process_name(pid) {
            if !names.iter().any(|n: &String| n == &name) {
                names.push(name);
            }
        }
    }
    Some(names)
}

#[cfg(windows)]
fn mic_in_use_windows() -> Option<bool> {
    let self_exe = std::env::current_exe().ok()?.to_string_lossy().to_string();
    let entries = read_mic_consent_entries();
    let (exe_paths, families) = running_processes_and_packages();
    Some(entries.iter().any(|e| mic_entry_active(e, &exe_paths, &families, &self_exe)))
}

#[cfg(windows)]
fn read_mic_consent_entries() -> Vec<MicConsentEntry> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::ERROR_NO_MORE_ITEMS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
        REG_VALUE_TYPE,
    };

    const ROOT: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

    unsafe fn open_sub(parent: HKEY, name: &str) -> Option<HKEY> {
        let mut h = HKEY::default();
        let r = unsafe { RegOpenKeyExW(parent, &HSTRING::from(name), Some(0), KEY_READ, &mut h) };
        if r.is_ok() {
            Some(h)
        } else {
            None
        }
    }

    unsafe fn read_qword(key: HKEY, name: &str) -> Option<i64> {
        let mut ty = REG_VALUE_TYPE::default();
        let mut data = 0u64;
        let mut len = std::mem::size_of::<u64>() as u32;
        let r = unsafe {
            RegQueryValueExW(
                key,
                &HSTRING::from(name),
                None,
                Some(&mut ty),
                Some(&mut data as *mut u64 as *mut u8),
                Some(&mut len),
            )
        };
        if r.is_err() {
            None
        } else {
            Some(data as i64)
        }
    }

    unsafe fn enum_keys(key: HKEY) -> Vec<String> {
        let mut out = Vec::new();
        for i in 0..512u32 {
            let mut name = [0u16; 512];
            let mut name_len = name.len() as u32;
            let r = unsafe {
                RegEnumKeyExW(
                    key,
                    i,
                    Some(windows::core::PWSTR(name.as_mut_ptr())),
                    &mut name_len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if r == ERROR_NO_MORE_ITEMS {
                break;
            }
            if r.is_err() {
                break;
            }
            out.push(String::from_utf16_lossy(&name[..name_len as usize]));
        }
        out
    }

    let Some(root) = (unsafe { open_sub(HKEY_CURRENT_USER, ROOT) }) else {
        return Vec::new();
    };
    let mut entries = Vec::new();

    for name in unsafe { enum_keys(root) } {
        if name.eq_ignore_ascii_case("NonPackaged") {
            continue;
        }
        if let Some(sub) = unsafe { open_sub(root, &name) } {
            let stop = unsafe { read_qword(sub, "LastUsedTimeStop") }.unwrap_or(-1);
            entries.push(MicConsentEntry { key_name: name, last_used_stop: stop, non_packaged: false });
            unsafe {
                let _ = RegCloseKey(sub);
            }
        }
    }

    if let Some(np) = unsafe { open_sub(root, "NonPackaged") } {
        for name in unsafe { enum_keys(np) } {
            if let Some(sub) = unsafe { open_sub(np, &name) } {
                let stop = unsafe { read_qword(sub, "LastUsedTimeStop") }.unwrap_or(-1);
                entries.push(MicConsentEntry { key_name: name, last_used_stop: stop, non_packaged: true });
                unsafe {
                    let _ = RegCloseKey(sub);
                }
            }
        }
        unsafe {
            let _ = RegCloseKey(np);
        }
    }

    unsafe {
        let _ = RegCloseKey(root);
    }
    entries
}

#[cfg(windows)]
fn running_processes_and_packages() -> (Vec<String>, Vec<String>) {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::ProcessStatus::EnumProcesses;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let mut pids = vec![0u32; 4096];
    let mut needed = 0u32;
    if unsafe {
        EnumProcesses(
            pids.as_mut_ptr(),
            (pids.len() * std::mem::size_of::<u32>()) as u32,
            &mut needed,
        )
    }
    .is_err()
    {
        return (Vec::new(), Vec::new());
    }
    let n = (needed as usize) / std::mem::size_of::<u32>();
    pids.truncate(n.min(pids.len()));

    let mut exe_paths = Vec::new();
    let mut families = Vec::new();
    for pid in pids.into_iter().filter(|p| *p != 0) {
        let Ok(h) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
            continue;
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        if unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.is_ok() {
            exe_paths.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        let mut fam = [0u16; 256];
        let mut fam_len = fam.len() as u32;
        if unsafe {
            windows::Win32::Storage::Packaging::Appx::GetPackageFamilyName(h, &mut fam_len, Some(PWSTR(fam.as_mut_ptr())))
        }
        .is_ok()
            && fam_len > 1
        {
            families.push(String::from_utf16_lossy(&fam[..(fam_len as usize).saturating_sub(1)]));
        }
        let _ = unsafe { CloseHandle(h) };
    }
    (exe_paths, families)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watching_requires_idle_threshold() {
        let mut s = WatchingHold::default();
        let t0 = Instant::now();
        // 前台出声但正在打字：不算在看
        assert!(!update_watching(&mut s, true, 10, 45, 30, t0));
        assert!(!s.watching);
        // 空闲达标：进入
        assert!(update_watching(&mut s, true, 45, 45, 30, t0 + Duration::from_secs(1)));
        assert!(s.watching);
    }

    #[test]
    fn watching_hold_keeps_state_through_brief_silence() {
        let mut s = WatchingHold::default();
        let t0 = Instant::now();
        assert!(update_watching(&mut s, true, 60, 45, 30, t0));
        // 无声 10 秒：不退出
        assert!(update_watching(&mut s, false, 70, 45, 30, t0 + Duration::from_secs(10)));
        assert!(s.watching);
        // 又出声：清掉无声计时
        assert!(update_watching(&mut s, true, 80, 45, 30, t0 + Duration::from_secs(15)));
        assert!(s.quiet_since.is_none());
        // 无声满 30 秒：退出
        let t1 = t0 + Duration::from_secs(20);
        assert!(update_watching(&mut s, false, 90, 45, 30, t1));
        assert!(!update_watching(&mut s, false, 100, 45, 30, t1 + Duration::from_secs(30)));
        assert!(!s.watching);
    }

    #[test]
    fn decode_nonpackaged_path() {
        assert_eq!(
            decode_nonpackaged_key(r"C:#Program Files#App#app.exe"),
            r"C:\Program Files\App\app.exe"
        );
    }

    #[test]
    fn mic_ignores_stopped_or_dead_or_self() {
        let running = vec![r"C:\Apps\Zoom.exe".into(), r"C:\Windows\System32\foo.exe".into()];
        let families = vec!["Microsoft.Teams_8wekyb3d8bbwe".into()];
        let self_exe = r"C:\Perch\perch.exe";

        assert!(!mic_entry_active(
            &MicConsentEntry { key_name: r"C:#Apps#Zoom.exe".into(), last_used_stop: 123, non_packaged: true },
            &running,
            &families,
            self_exe,
        ));
        assert!(!mic_entry_active(
            &MicConsentEntry { key_name: r"C:#Apps#Dead.exe".into(), last_used_stop: 0, non_packaged: true },
            &running,
            &families,
            self_exe,
        ));
        assert!(!mic_entry_active(
            &MicConsentEntry { key_name: r"C:#Perch#perch.exe".into(), last_used_stop: 0, non_packaged: true },
            &running,
            &families,
            self_exe,
        ));
        assert!(mic_entry_active(
            &MicConsentEntry { key_name: r"C:#Apps#Zoom.exe".into(), last_used_stop: 0, non_packaged: true },
            &running,
            &families,
            self_exe,
        ));
        assert!(mic_entry_active(
            &MicConsentEntry {
                key_name: "Microsoft.Teams_8wekyb3d8bbwe".into(),
                last_used_stop: 0,
                non_packaged: false
            },
            &running,
            &families,
            self_exe,
        ));
        assert!(!mic_entry_active(
            &MicConsentEntry { key_name: "Dead.App_123".into(), last_used_stop: 0, non_packaged: false },
            &running,
            &families,
            self_exe,
        ));
    }

    #[test]
    fn foreground_playing_matches_by_process_name() {
        let playing = vec!["chrome".into(), "msedge".into()];
        assert!(foreground_is_playing(Some("chrome"), &playing));
        assert!(foreground_is_playing(Some("Chrome.exe"), &playing));
        assert!(!foreground_is_playing(Some("code"), &playing));
        assert!(!foreground_is_playing(None, &playing));
    }
}
