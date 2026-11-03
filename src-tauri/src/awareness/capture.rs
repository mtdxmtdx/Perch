//! 按需截屏：目标窗口追踪、缩放、PNG、WGC 单帧。
//!
//! Windows 下用 MIT 的 `windows-capture`（依赖与本仓库相同的 `windows` 0.61）做 WGC；
//! 门禁与缩放/编码是纯逻辑，可在任意平台单测。选库原因见 PR：比手写 WinRT/D3D11 帧池更稳，依赖仍少。

use crate::config::AwarenessParams;
use crate::awareness::gate::{self, CaptureDeny, WindowInfo};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::Write;
use std::sync::Mutex;
use std::time::Instant;

/// 截图范围
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Window,
    Screen,
}

impl Scope {
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(|x| x.trim().to_lowercase()).as_deref() {
            Some("screen") | Some("monitor") | Some("display") => Self::Screen,
            _ => Self::Window,
        }
    }
}

/// 一次成功截图（原图只在内存）
#[derive(Clone, Debug)]
pub struct Shot {
    pub png: Vec<u8>,
    pub thumb_png: Vec<u8>,
    pub process: String,
    pub width: u32,
    pub height: u32,
    /// 缩放后画面的 64×36 HSV 网格（主动看屏变化比对用）
    pub hsv_grid: Vec<f32>,
}

/// 控制器记录的最近非 Perch 前台窗口
#[derive(Clone, Debug, Default)]
pub struct TargetRecord {
    pub hwnd: isize,
    pub pid: u32,
    pub process: String,
    pub at: Option<Instant>,
}

static TARGET: Mutex<TargetRecord> = Mutex::new(TargetRecord {
    hwnd: 0,
    pid: 0,
    process: String::new(),
    at: None,
});

static SEEING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 由 `pet/controller` 在发现非本进程前台窗口时调用
pub fn note_foreground(hwnd: isize, pid: u32, process: &str) {
    if hwnd == 0 || pid == 0 || process.is_empty() {
        return;
    }
    let mut t = TARGET.lock().unwrap();
    t.hwnd = hwnd;
    t.pid = pid;
    t.process = process.trim().trim_end_matches(".exe").to_lowercase();
    t.at = Some(Instant::now());
}

pub fn target_record() -> TargetRecord {
    TARGET.lock().unwrap().clone()
}

pub fn set_seeing(on: bool) {
    SEEING.store(on, std::sync::atomic::Ordering::SeqCst);
}

pub fn is_seeing() -> bool {
    SEEING.load(std::sync::atomic::Ordering::SeqCst)
}

/// 面积平均缩放到长边不超过 `long_side`（0 或极大值表示不缩放）
pub fn downscale_area_average(rgba: &[u8], w: u32, h: u32, long_side: u32) -> (Vec<u8>, u32, u32) {
    assert_eq!(rgba.len(), (w as usize) * (h as usize) * 4);
    if w == 0 || h == 0 {
        return (Vec::new(), 0, 0);
    }
    let long = w.max(h);
    if long_side == 0 || long <= long_side {
        return (rgba.to_vec(), w, h);
    }
    let scale = long_side as f64 / long as f64;
    let nw = ((w as f64) * scale).round().max(1.0) as u32;
    let nh = ((h as f64) * scale).round().max(1.0) as u32;
    let mut out = vec![0u8; (nw as usize) * (nh as usize) * 4];
    for y in 0..nh {
        let y0 = (y as u64 * h as u64) / nh as u64;
        let y1 = ((y as u64 + 1) * h as u64) / nh as u64;
        let y1 = y1.max(y0 + 1);
        for x in 0..nw {
            let x0 = (x as u64 * w as u64) / nw as u64;
            let x1 = ((x as u64 + 1) * w as u64) / nw as u64;
            let x1 = x1.max(x0 + 1);
            let mut sum = [0u64; 4];
            let mut n = 0u64;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let i = ((yy as u32 * w + xx as u32) as usize) * 4;
                    for c in 0..4 {
                        sum[c] += rgba[i + c] as u64;
                    }
                    n += 1;
                }
            }
            let o = ((y * nw + x) as usize) * 4;
            for c in 0..4 {
                out[o + c] = ((sum[c] + n / 2) / n) as u8;
            }
        }
    }
    (out, nw, nh)
}

/// 裸 RGBA8 → PNG（IHDR/IDAT/IEND），压缩用 flate2
pub fn encode_png_rgba(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    if w == 0 || h == 0 {
        return Err("空图像".into());
    }
    if rgba.len() != (w as usize) * (h as usize) * 4 {
        return Err("RGBA 长度与宽高不符".into());
    }
    let mut raw = Vec::with_capacity(((w as usize) * 4 + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0); // filter None
        let row = y * (w as usize) * 4;
        raw.extend_from_slice(&rgba[row..row + (w as usize) * 4]);
    }
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(&raw).map_err(|e| e.to_string())?;
    let compressed = enc.finish().map_err(|e| e.to_string())?;

    let mut out = Vec::new();
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]); // signature
    write_chunk(&mut out, b"IHDR", &{
        let mut d = Vec::new();
        d.extend_from_slice(&w.to_be_bytes());
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
        d
    });
    write_chunk(&mut out, b"IDAT", &compressed);
    write_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

fn write_chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(ty);
    out.extend_from_slice(data);
    let mut c = crc32_init();
    c = crc32_update(c, ty);
    c = crc32_update(c, data);
    out.extend_from_slice(&crc32_finalize(c).to_be_bytes());
}

fn crc32_init() -> u32 {
    0xffff_ffff
}
fn crc32_update(mut c: u32, data: &[u8]) -> u32 {
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            if c & 1 != 0 {
                c = (c >> 1) ^ 0xEDB88320;
            } else {
                c >>= 1;
            }
        }
    }
    c
}
fn crc32_finalize(c: u32) -> u32 {
    !c
}

/// DRM / 受保护内容：几乎全黑
pub fn is_mostly_black(rgba: &[u8], threshold: u8, black_ratio: f32) -> bool {
    if rgba.len() < 4 {
        return true;
    }
    let mut black = 0usize;
    let n = rgba.len() / 4;
    for px in rgba.chunks_exact(4) {
        if px[0] <= threshold && px[1] <= threshold && px[2] <= threshold {
            black += 1;
        }
    }
    (black as f32) / (n as f32) >= black_ratio
}

pub fn prepare_shot(bgra_or_rgba: &[u8], w: u32, h: u32, long_side: u32, is_bgra: bool) -> Result<Shot, String> {
    let mut rgba = bgra_or_rgba.to_vec();
    if is_bgra {
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
    }
    if is_mostly_black(&rgba, 8, 0.98) {
        return Err("画面受保护，看不到".into());
    }
    let (scaled, sw, sh) = downscale_area_average(&rgba, w, h, long_side);
    let png = encode_png_rgba(&scaled, sw, sh)?;
    let (thumb, tw, th) = downscale_area_average(&scaled, sw, sh, 320);
    let thumb_png = encode_png_rgba(&thumb, tw, th)?;
    let hsv_grid = crate::awareness::cuts::hsv_grid_from_rgba(&scaled, sw, sh);
    Ok(Shot { png, thumb_png, process: String::new(), width: sw, height: sh, hsv_grid })
}

/// 解析目标窗口；失败返回门禁错误
pub fn resolve_target() -> Result<WindowInfo, CaptureDeny> {
    #[cfg(windows)]
    {
        resolve_target_windows()
    }
    #[cfg(not(windows))]
    {
        let t = target_record();
        if t.hwnd == 0 {
            return Err(CaptureDeny::NoTarget);
        }
        Ok(WindowInfo { hwnd: t.hwnd, process: t.process, title: String::new() })
    }
}

#[cfg(windows)]
fn resolve_target_windows() -> Result<WindowInfo, CaptureDeny> {
    use crate::pet::win;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, IsIconic, IsWindow};

    let t = target_record();
    if t.hwnd == 0 || !unsafe { IsWindow(Some(win::hwnd(t.hwnd))).as_bool() } {
        return Err(CaptureDeny::NoTarget);
    }
    if unsafe { IsIconic(win::hwnd(t.hwnd)).as_bool() } || !win::is_visible(t.hwnd) {
        return Err(CaptureDeny::Minimized);
    }
    if win::is_cloaked(t.hwnd) {
        return Err(CaptureDeny::Cloaked);
    }
    let title = {
        let mut buf = [0u16; 512];
        let n = unsafe { GetWindowTextW(win::hwnd(t.hwnd), &mut buf) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    };
    let process = if t.process.is_empty() {
        win::process_name(t.pid).unwrap_or_default()
    } else {
        t.process.clone()
    };
    Ok(WindowInfo { hwnd: t.hwnd, process, title })
}

/// 枚举某显示器上可见、未最小化、未 cloaked 的顶层窗口（不含工具窗）
#[cfg(windows)]
pub fn visible_windows_on_monitor(monitor: isize) -> Vec<WindowInfo> {
    use crate::pet::win;
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowLongPtrW, GetWindowTextW, GWL_EXSTYLE, GWL_STYLE, WS_EX_TOOLWINDOW,
        WS_VISIBLE,
    };

    struct Data {
        monitor: isize,
        self_pid: u32,
        out: Vec<WindowInfo>,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let data = &mut *(lp.0 as *mut Data);
        let raw = hwnd.0 as isize;
        if !win::is_visible(raw) || win::is_cloaked(raw) {
            return BOOL(1);
        }
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        if style & WS_VISIBLE.0 == 0 {
            return BOOL(1);
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return BOOL(1);
        }
        if win::monitor_of_window(raw) != data.monitor {
            return BOOL(1);
        }
        let pid = win::window_pid(raw);
        if pid == data.self_pid {
            return BOOL(1);
        }
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut buf);
        let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        let process = win::process_name(pid).unwrap_or_default();
        if process.is_empty() && title.is_empty() {
            return BOOL(1);
        }
        data.out.push(WindowInfo { hwnd: raw, process, title });
        BOOL(1)
    }
    let mut data = Data { monitor, self_pid: std::process::id(), out: Vec::new() };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut data as *mut _ as isize));
    }
    data.out
}

/// 截一张：先过门禁，再 WGC
pub fn capture(scope: Scope, p: &AwarenessParams) -> Result<Shot, String> {
    let target = resolve_target().map_err(|d| d.message(""))?;
    gate::gate(&target, p).map_err(|d| d.message(&target.process))?;

    #[cfg(windows)]
    {
        capture_windows(scope, p, &target)
    }
    #[cfg(not(windows))]
    {
        let _ = (scope, p, target);
        Err("看屏只在 Windows 上可用".into())
    }
}

#[cfg(windows)]
fn capture_windows(scope: Scope, p: &AwarenessParams, target: &WindowInfo) -> Result<Shot, String> {
    use crate::pet::win;

    match scope {
        Scope::Window => {
            let (w, h, bgra) = wgc_grab_window(target.hwnd)?;
            let mut shot = prepare_shot(&bgra, w, h, p.see_screen_long_side, true)?;
            shot.process = target.process.clone();
            Ok(shot)
        }
        Scope::Screen => {
            let mon = win::monitor_of_window(target.hwnd);
            if mon == 0 {
                return Err("找不到目标窗口所在的显示器".into());
            }
            for w in visible_windows_on_monitor(mon) {
                if let Err(d) = gate::gate(&w, p) {
                    return Err(d.message(&w.process));
                }
            }
            let _guard = ExcludeFromCaptureGuard::apply();
            let (w, h, bgra) = wgc_grab_monitor(mon)?;
            let mut shot = prepare_shot(&bgra, w, h, p.see_screen_long_side, true)?;
            shot.process = target.process.clone();
            Ok(shot)
        }
    }
}

#[cfg(windows)]
struct ExcludeFromCaptureGuard {
    restored: Vec<(isize, u32)>,
}

#[cfg(windows)]
impl ExcludeFromCaptureGuard {
    fn apply() -> Self {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowDisplayAffinity, SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
        };
        use crate::pet::win;
        let mut restored = Vec::new();
        // 尽最大努力：枚举本进程顶层窗
        let self_pid = std::process::id();
        for hwnd in enum_self_top_level(self_pid) {
            let mut prev = 0u32;
            let _ = unsafe { GetWindowDisplayAffinity(win::hwnd(hwnd), &mut prev) };
            if unsafe { SetWindowDisplayAffinity(win::hwnd(hwnd), WDA_EXCLUDEFROMCAPTURE) }.is_ok() {
                restored.push((hwnd, prev));
            }
        }
        // 给合成器一点时间
        std::thread::sleep(std::time::Duration::from_millis(30));
        let _ = WDA_NONE; // keep import used in Drop
        Self { restored }
    }
}

#[cfg(windows)]
impl Drop for ExcludeFromCaptureGuard {
    fn drop(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowDisplayAffinity, WINDOW_DISPLAY_AFFINITY,
        };
        use crate::pet::win;
        for (hwnd, prev) in self.restored.drain(..) {
            let _ = unsafe { SetWindowDisplayAffinity(win::hwnd(hwnd), WINDOW_DISPLAY_AFFINITY(prev)) };
        }
    }
}

#[cfg(windows)]
fn enum_self_top_level(self_pid: u32) -> Vec<isize> {
    use crate::pet::win;
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::EnumWindows;
    struct D { pid: u32, out: Vec<isize> }
    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let d = &mut *(lp.0 as *mut D);
        let raw = hwnd.0 as isize;
        if win::window_pid(raw) == d.pid {
            d.out.push(raw);
        }
        BOOL(1)
    }
    let mut d = D { pid: self_pid, out: Vec::new() };
    unsafe { let _ = EnumWindows(Some(cb), LPARAM(&mut d as *mut _ as isize)); }
    d.out
}

#[cfg(windows)]
fn wgc_grab_window(hwnd: isize) -> Result<(u32, u32, Vec<u8>), String> {
    use windows_capture::window::Window;
    let window = Window::from_raw_hwnd(hwnd as *mut std::ffi::c_void);
    wgc_grab(window)
}

#[cfg(windows)]
fn wgc_grab_monitor(hmonitor: isize) -> Result<(u32, u32, Vec<u8>), String> {
    use windows_capture::monitor::Monitor;
    let mon = Monitor::from_raw_hmonitor(hmonitor as *mut std::ffi::c_void);
    wgc_grab(mon)
}

#[cfg(windows)]
fn wgc_grab<T>(item: T) -> Result<(u32, u32, Vec<u8>), String>
where
    T: windows_capture::settings::TryIntoCaptureItemWithType + Send + 'static,
{
    use std::sync::mpsc;
    use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
    use windows_capture::frame::Frame;
    use windows_capture::graphics_capture_api::InternalCaptureControl;
    use windows_capture::settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    };

    type Tx = mpsc::Sender<Result<(u32, u32, Vec<u8>), String>>;

    struct Once {
        tx: Option<Tx>,
    }
    impl GraphicsCaptureApiHandler for Once {
        type Flags = Tx;
        type Error = Box<dyn std::error::Error + Send + Sync>;
        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            Ok(Self { tx: Some(ctx.flags) })
        }
        fn on_frame_arrived(
            &mut self,
            frame: &mut Frame,
            capture_control: InternalCaptureControl,
        ) -> Result<(), Self::Error> {
            let w = frame.width();
            let h = frame.height();
            let result = (|| {
                let mut buf = frame.buffer().map_err(|e| e.to_string())?;
                let pixels = buf.as_nopadding_buffer().map_err(|e| e.to_string())?.to_vec();
                Ok::<_, String>((w, h, pixels))
            })();
            if let Some(tx) = self.tx.take() {
                let _ = tx.send(result);
            }
            capture_control.stop();
            Ok(())
        }
        fn on_closed(&mut self) -> Result<(), Self::Error> {
            if let Some(tx) = self.tx.take() {
                let _ = tx.send(Err("捕获会话已关闭".into()));
            }
            Ok(())
        }
    }

    let (tx, rx) = mpsc::channel();
    let settings = Settings::new(
        item,
        CursorCaptureSettings::Default,
        DrawBorderSettings::Default,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        tx,
    );

    Once::start(settings).map_err(|e| format!("启动看屏失败：{e}"))?;
    rx.recv().map_err(|_| "未收到画面".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_halves_long_side() {
        // 4x2 red image
        let mut rgba = vec![0u8; 4 * 2 * 4];
        for px in rgba.chunks_exact_mut(4) {
            px[0] = 255;
            px[3] = 255;
        }
        let (out, w, h) = downscale_area_average(&rgba, 4, 2, 2);
        assert_eq!((w, h), (2, 1));
        assert_eq!(out.len(), 2 * 1 * 4);
        assert_eq!(out[0], 255);
    }

    #[test]
    fn png_roundtrip_header() {
        let rgba = vec![0u8, 128, 255, 255, 255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255];
        // fix CRC first - this test will validate after CRC fix
        let png = encode_png_rgba(&rgba, 2, 2).expect("png");
        assert!(png.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]));
        assert!(png.len() > 50);
    }

    #[test]
    fn black_frame_detected() {
        let rgba = vec![0u8; 16 * 16 * 4];
        assert!(is_mostly_black(&rgba, 8, 0.98));
        let mut mixed = rgba;
        for i in 0..32 {
            mixed[i * 4] = 200;
        }
        assert!(!is_mostly_black(&mixed, 8, 0.98));
    }

    /// 真实截一次前台窗口（需要桌面会话）：cargo test -- --ignored wgc_grabs
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn wgc_grabs_foreground_window() {
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        let hwnd = unsafe { GetForegroundWindow() }.0 as isize;
        assert_ne!(hwnd, 0);
        let (w, h, bgra) = wgc_grab_window(hwnd).expect("grab");
        assert!(w > 0 && h > 0);
        assert_eq!(bgra.len(), (w * h * 4) as usize);
        let shot = prepare_shot(&bgra, w, h, 1280, true).expect("shot");
        assert!(shot.width.max(shot.height) <= 1280);
        let dir = std::env::temp_dir().join("perch-wgc-test");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("shot.png"), &shot.png).unwrap();
        std::fs::write(dir.join("thumb.png"), &shot.thumb_png).unwrap();
        eprintln!("{}x{} -> {}x{} {:?}", w, h, shot.width, shot.height, dir);
    }

    #[test]
    fn scope_parse() {
        assert_eq!(Scope::parse(None), Scope::Window);
        assert_eq!(Scope::parse(Some("screen")), Scope::Screen);
        assert_eq!(Scope::parse(Some("WINDOW")), Scope::Window);
    }
}
