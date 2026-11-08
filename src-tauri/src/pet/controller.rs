//! 桌宠控制线程（约 60Hz）：物理、自主行为、拖拽、鼠标穿透、全屏隐藏。
//! 位置用"身体中心 x + 脚底 y"（物理像素）表示，每帧换算成窗口左上角。

use super::fullscreen::FsMon;
use super::surfaces::{self, Interval, Snapshot};
use super::win::{self, Monitor, Rect};
use super::{fullscreen, hittest, screens, SharedState};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};

const TICK: Duration = Duration::from_millis(16);
const SNAPSHOT_EVERY: Duration = Duration::from_millis(100);
const FULLSCREEN_EVERY: Duration = Duration::from_millis(400);
/// 多久检查一次桌宠、气泡、状态标签有没有被别的置顶窗口盖住
const ZORDER_EVERY: Duration = Duration::from_secs(1);
/// 全屏结束这么久后才回原来那块屏（切出游戏又切回去时不来回跳）
const FS_RETURN_AFTER: Duration = Duration::from_secs(5);
/// 记下的位置变了以后，最快多久写一次配置文件
const PLACE_SAVE_EVERY: Duration = Duration::from_secs(30);
/// 宿主窗口静止这么久后才检查脚下是否被遮挡，避免拖窗口时误判
const HOST_SETTLE: Duration = Duration::from_millis(200);
/// 以下数值都对应默认大小（高 156px），实际按桌宠高度等比缩放
const GRAVITY: f64 = 2600.0;
const WALK_SPEED: f64 = 70.0;
const MAX_THROW: f64 = 1800.0;
const DRAG_THRESHOLD: i32 = 4;
/// 窗口一般顶到屏幕上部，跳跃高度要够大才够得着
const MAX_JUMP_RISE: f64 = 1300.0;
const MAX_JUMP_DX: f64 = 900.0;

enum Ground {
    Work,
    Window { hwnd: isize, rect: Rect, changed_at: Instant },
}

#[derive(Clone, Copy)]
enum Act {
    Idle,
    Walk { dir: f64 },
    Sleep,
    /// 坐在窗沿上（只在窗口顶边，且皮肤有坐的动画时）
    Sit,
    Wave,
    Land,
}

enum Mode {
    Grounded { ground: Ground, act: Act, until: Instant },
    Air { vx: f64, vy: f64 },
    Drag { gx: f64, gy: f64, hist: VecDeque<(Instant, f64, f64)> },
}

struct Ctl {
    app: AppHandle,
    window: WebviewWindow,
    hwnd: isize,
    pid: u32,
    shared: SharedState,
    cx: f64,
    fy: f64,
    mode: Mode,
    /// 按下但还没判定成拖拽时的光标位置
    press: Option<(i32, i32)>,
    snap: Snapshot,
    fs_hidden: bool,
    /// 桌宠所在显示器上有没有全屏程序（不管设置里要不要因此隐藏，主动聊天也要用）
    fs_now: bool,
    last_fs_check: Instant,
    /// 有没有哪块屏在全屏（桌宠因此换了屏时，主动聊天照样要安静）
    fs_any: bool,
    /// 因为全屏换了屏：原来在哪块屏、横向位置；全屏结束后回去
    fs_origin: Option<(String, f64)>,
    /// 原来那块屏不再全屏的时刻
    fs_clear_since: Option<Instant>,
    /// 活动范围：只在这块显示器上（设备名）；None 所有显示器
    scope: Option<String>,
    /// 上次写进配置文件的位置
    saved_place: Option<(String, f64)>,
    last_place_check: Instant,
    last_place_save: Instant,
    last_z_check: Instant,
    /// 上次盖住桌宠的窗口（同一个只记一次日志）
    last_cover: String,
    shown: bool,
    anim: &'static str,
    /// 上次告诉前端的水平朝向（1 右，-1 左，0 还没动过）
    facing: f64,
    ignore_cursor: Option<bool>,
    last_pos: Option<(i32, i32)>,
    bubble_pos: Option<(i32, i32)>,
    bubble_anchor: Option<crate::windows::BubbleAnchor>,
    /// 状态标签窗口；当前位置（None = 停在屏幕外）；是否放在右侧
    status: Option<tauri::WebviewWindow>,
    status_pos: Option<(i32, i32)>,
    status_right: bool,
    /// 环境变量 PERCH_DEBUG=1 时打印状态切换和可站面
    debug: bool,
    /// 环境变量 PERCH_DEBUG_SIT=1 时，站在窗口顶边一有机会就坐（测试用）
    force_sit: bool,
    last_debug: Instant,
}

fn rand_secs(lo: f64, hi: f64) -> Duration {
    Duration::from_secs_f64(lo + fastrand::f64() * (hi - lo))
}

fn throw_velocity(hist: &VecDeque<(Instant, f64, f64)>) -> (f64, f64) {
    match (hist.front(), hist.back()) {
        (Some(a), Some(b)) => {
            let dt = (b.0 - a.0).as_secs_f64();
            if dt < 0.01 {
                (0.0, 0.0)
            } else {
                ((b.1 - a.1) / dt, (b.2 - a.2) / dt)
            }
        }
        _ => (0.0, 0.0),
    }
}

impl Ctl {
    fn new(app: AppHandle, window: WebviewWindow, hwnd: isize, shared: SharedState) -> Self {
        let pid = std::process::id();
        let mut ctl = Self {
            app,
            window,
            hwnd,
            pid,
            shared,
            cx: 0.0,
            fy: 0.0,
            mode: Mode::Air { vx: 0.0, vy: 0.0 },
            press: None,
            snap: surfaces::snapshot(pid, 156),
            fs_hidden: false,
            fs_now: false,
            last_fs_check: Instant::now(),
            fs_any: false,
            fs_origin: None,
            fs_clear_since: None,
            scope: None,
            saved_place: None,
            last_place_check: Instant::now(),
            last_place_save: Instant::now(),
            last_z_check: Instant::now(),
            last_cover: String::new(),
            shown: false,
            anim: "",
            facing: 0.0,
            ignore_cursor: None,
            last_pos: None,
            bubble_pos: None,
            bubble_anchor: None,
            status: None,
            status_pos: None,
            status_right: true,
            debug: std::env::var_os("PERCH_DEBUG").is_some(),
            force_sit: std::env::var_os("PERCH_DEBUG_SIT").is_some(),
            last_debug: Instant::now(),
        };
        ctl.scope = ctl.shared.lock().unwrap().screen_scope.clone();
        ctl.place_initial(1.0, true);
        ctl
    }

    /// 能去的显示器：限定了某一块（而且它还接着）就只有它
    fn allowed_monitors(&self) -> Vec<&Monitor> {
        let all = self.snap.monitors.iter();
        match &self.scope {
            Some(name) if self.snap.monitors.iter().any(|m| &m.name == name) => all.filter(|m| &m.name == name).collect(),
            _ => all.collect(),
        }
    }

    fn allowed(&self, m: &Monitor) -> bool {
        self.allowed_monitors().iter().any(|a| a.name == m.name)
    }

    /// 脚下这块屏（不在任何屏里时取最近的）
    fn feet_monitor(&self) -> Option<&Monitor> {
        screens::at_or_nearest(&self.snap.monitors, self.cx, self.fy - 1.0)
    }

    /// 现在在哪块屏、横向位置
    fn place(&self) -> Option<(String, f64)> {
        self.feet_monitor().map(|m| (m.name.clone(), screens::x_ratio(m, self.cx)))
    }

    /// 从这块屏的上方掉下来，横向放在 ratio 处
    fn drop_onto(&mut self, m: &Monitor, ratio: f64, k: f64) {
        self.cx = screens::x_at(m, ratio, 60.0 * k);
        self.fy = m.work.b as f64 - 260.0 * k;
        self.mode = Mode::Air { vx: 0.0, vy: 0.0 };
    }

    /// 启动时（use_saved）回到上次待的地方；「放回任务栏」时落到主屏中间。限定了显示器时只在那块屏
    fn place_initial(&mut self, k: f64, use_saved: bool) {
        let saved = if use_saved { self.app.state::<crate::AppState>().config.lock().unwrap().pet_place.clone() } else { None };
        let allowed: Vec<Monitor> = self.allowed_monitors().into_iter().cloned().collect();
        let target = saved
            .and_then(|p| allowed.iter().find(|m| m.name == p.screen).map(|m| (m.clone(), p.x)))
            .or_else(|| allowed.iter().find(|m| m.primary).or(allowed.first()).map(|m| (m.clone(), 0.5)));
        match target {
            Some((m, ratio)) => self.drop_onto(&m, ratio, k),
            None => self.mode = Mode::Air { vx: 0.0, vy: 0.0 },
        }
    }

    /// 掉落时能不能落在这段面上：限定了显示器时只落在那块屏里的面
    fn interval_allowed(&self, iv: &Interval) -> bool {
        self.scope.is_none()
            || screens::at_or_nearest(&self.snap.monitors, (iv.x1 + iv.x2) / 2.0, iv.y).is_some_and(|m| self.allowed(m))
    }

    fn grounded(&self, iv: &Interval, act: Act, dur: Duration) -> Mode {
        let now = Instant::now();
        let ground = if iv.hwnd == 0 {
            Ground::Work
        } else {
            match surfaces::host_rect(iv.hwnd) {
                Some(rect) => Ground::Window { hwnd: iv.hwnd, rect, changed_at: now },
                None => Ground::Work,
            }
        };
        Mode::Grounded { ground, act, until: now + dur }
    }

    /// 宿主窗口上包含脚下位置的那段可站线段（已按宿主当前位置平移）
    fn host_interval(&self, hwnd: isize, rect: Rect) -> Option<(f64, f64)> {
        self.snap
            .intervals
            .iter()
            .filter(|iv| iv.hwnd == hwnd)
            .map(|iv| {
                let off = (rect.l - iv.host_left) as f64;
                (iv.x1 + off, iv.x2 + off)
            })
            .find(|&(a, b)| self.cx >= a - 2.0 && self.cx <= b + 2.0)
    }

    fn work_bounds(&self) -> Option<(f64, f64, f64)> {
        self.feet_monitor().map(|m| (m.work.l as f64, m.work.r as f64, m.work.b as f64))
    }

    /// 本帧下落时穿过的最高可站面
    fn landing(&self, prev_fy: f64) -> Option<Interval> {
        self.snap
            .intervals
            .iter()
            .filter(|iv| prev_fy <= iv.y + 0.5 && self.fy >= iv.y && self.cx >= iv.x1 && self.cx <= iv.x2)
            .filter(|iv| self.interval_allowed(iv))
            .min_by(|a, b| a.y.total_cmp(&b.y))
            .copied()
    }

    /// 掉出所有可站面时（比如显示器之间的缝隙），落到最近那块（能去的）显示器的任务栏上
    fn fallback_landing(&mut self) -> Option<Interval> {
        let m = screens::at_or_nearest(self.allowed_monitors(), self.cx, self.fy)?.clone();
        if self.fy < m.work.b as f64 {
            return None;
        }
        self.cx = self.cx.clamp(m.work.l as f64 + 20.0, m.work.r as f64 - 20.0);
        Some(Interval { hwnd: 0, y: m.work.b as f64, x1: m.work.l as f64, x2: m.work.r as f64, host_left: 0 })
    }

    /// 松手时脚刚好陷在某个面里（比如放在标题栏上），就站上去
    fn drop_target(&self, h: f64) -> Option<Interval> {
        self.snap
            .intervals
            .iter()
            .filter(|iv| self.cx >= iv.x1 && self.cx <= iv.x2 && self.fy >= iv.y - 2.0 && self.fy - iv.y <= h * 0.3)
            .filter(|iv| self.interval_allowed(iv))
            .min_by(|a, b| (self.fy - a.y).total_cmp(&(self.fy - b.y)))
            .copied()
    }

    /// 随机挑一个够得着的面，算出起跳速度
    fn plan_jump(&self, k: f64, w: f64, h: f64) -> Option<(f64, f64)> {
        let margin = w * 0.35;
        // 头不能顶出屏幕：最高点的脚底至少比起跳和落脚那两块屏的顶低一个身高
        let here_top = self.feet_monitor().map(|m| m.full.t).unwrap_or(0) as f64;
        let cands: Vec<(f64, f64, f64)> = self
            .snap
            .intervals
            .iter()
            .filter(|iv| self.interval_allowed(iv))
            .filter_map(|iv| {
                if iv.x2 - iv.x1 < 2.0 * margin + 10.0 {
                    return None;
                }
                if (iv.y - self.fy).abs() < 2.0 && self.cx >= iv.x1 && self.cx <= iv.x2 {
                    return None;
                }
                let tx = iv.x1 + margin + fastrand::f64() * (iv.x2 - iv.x1 - 2.0 * margin);
                let there_top = screens::at_or_nearest(&self.snap.monitors, tx, iv.y).map(|m| m.full.t).unwrap_or(0) as f64;
                let ceiling = here_top.max(there_top) + h + 4.0;
                let dx = (tx - self.cx).abs();
                let rise = self.fy - iv.y;
                let ok = iv.y - ceiling >= 20.0 * k
                    && dx >= 60.0 * k && dx <= MAX_JUMP_DX * k && rise <= MAX_JUMP_RISE * k && rise >= -1600.0 * k;
                ok.then_some((tx, iv.y, ceiling))
            })
            .collect();
        if cands.is_empty() {
            return None;
        }
        let (tx, ty, ceiling) = cands[fastrand::usize(..cands.len())];
        let g = GRAVITY * k;
        let apex = (self.fy.min(ty) - (70.0 + fastrand::f64() * 60.0) * k).max(ceiling);
        let vy = -(2.0 * g * (self.fy - apex)).sqrt();
        let t = -vy / g + (2.0 * (ty - apex) / g).sqrt();
        Some(((tx - self.cx) / t, vy))
    }

    #[allow(clippy::too_many_arguments)]
    fn follow_bubble(&mut self, left: i32, top: i32, w: f64, h: f64, m: &super::Metrics, size: (i32, i32), moved: bool) {
        if size.0 <= 0 || size.1 <= 0 {
            return;
        }
        let head_y = top as f64 + m.top_ratio * h;
        let work = self
            .feet_monitor()
            .map(|mon| (mon.work.l, mon.work.t, mon.work.r, mon.work.b))
            .unwrap_or((0, 0, 1920, 1080));
        let (pos, placement) =
            crate::windows::place_bubble((left, top, w as i32, h as i32), self.cx, head_y, size, work);
        if moved || self.bubble_pos != Some(pos) {
            if self.bubble_pos.is_none() {
                eprintln!(
                    "[pet] {} 气泡放在 ({}, {})，大小 {}×{}，{:?}；桌宠脚下 ({:.0}, {:.0})",
                    crate::logfile::stamp(),
                    pos.0,
                    pos.1,
                    size.0,
                    size.1,
                    placement,
                    self.cx,
                    self.fy
                );
            }
            self.bubble_pos = Some(pos);
            crate::windows::move_bubble(&self.app, pos);
            let anchor = crate::windows::BubbleAnchor {
                placement,
                tail_ratio: ((self.cx - pos.0 as f64) / size.0 as f64).clamp(0.08, 0.92),
            };
            if self.bubble_anchor.as_ref() != Some(&anchor) {
                let _ = self.app.emit_to(crate::windows::BUBBLE, "bubble-anchor", anchor.clone());
                self.bubble_anchor = Some(anchor);
            }
        }
    }

    /// 状态标签跟着桌宠；没有标签或桌宠隐藏时停到屏幕外（不用 hide/show，免得抢焦点）
    fn follow_status(&mut self, place: Option<(f64, f64, f64, (i32, i32))>) {
        let Some(win) = self.status.clone() else { return };
        let pos = place.map(|(head_y, foot_y, half_body, size)| {
            let work = self
                .snap
                .monitors
                .iter()
                .find(|mon| mon.full.contains_x(self.cx))
                .or(self.snap.monitors.first())
                .map(|mon| (mon.work.l, mon.work.t, mon.work.r, mon.work.b))
                .unwrap_or((0, 0, 1920, 1080));
            let (pos, right) = crate::windows::place_status(self.cx, head_y, foot_y, half_body, size, work, self.status_right);
            if right != self.status_right {
                self.status_right = right;
                let _ = self.app.emit_to(crate::windows::STATUS, "status-side", if right { "right" } else { "left" });
            }
            pos
        });
        if pos == self.status_pos {
            return;
        }
        self.status_pos = pos;
        let target = pos.unwrap_or_else(|| {
            let vs = win::virtual_screen();
            (vs.r + 4000, vs.b + 4000)
        });
        let _ = win.set_position(PhysicalPosition::new(target.0, target.1));
    }

    fn on_click(&mut self) {
        if let Mode::Grounded { act, until, .. } = &mut self.mode {
            *act = Act::Wave;
            *until = Instant::now() + Duration::from_millis(1200);
        }
        crate::windows::toggle_bubble(&self.app);
    }

    fn tick(&mut self, dt: f64) {
        let now = Instant::now();
        let (ready, work_mode, hide_fs, user_hidden, reset, pressed, metrics) = {
            let mut s = self.shared.lock().unwrap();
            let pressed = std::mem::take(&mut s.pointer_pressed);
            let reset = std::mem::take(&mut s.reset_requested);
            (s.ready, s.work_mode, s.hide_on_fullscreen, s.user_hidden, reset, pressed, s.metrics.clone())
        };
        let (speech, bubble, agent_anim) = {
            let mut s = self.shared.lock().unwrap();
            if s.speech_until.is_some_and(|t| now >= t) {
                s.speech = None;
                s.speech_until = None;
            }
            let moved = std::mem::take(&mut s.bubble_moved);
            (s.speech, s.bubble_visible.then_some((s.bubble_size, moved)), s.agent_anim)
        };
        if !ready {
            return;
        }
        let Some(rect) = win::window_rect(self.hwnd) else { return };
        let (w, h) = (rect.w() as f64, rect.h() as f64);
        let k = (h / 156.0).max(0.3);

        if now - self.snap.at >= SNAPSHOT_EVERY {
            self.snap = surfaces::snapshot(self.pid, rect.h());
            // 记录最近一个不属于 Perch 的前台窗口，供 see_screen 使用
            if let Some((hwnd, pid, name)) = foreground_non_self(self.pid) {
                crate::awareness::note_foreground(hwnd, pid, &name);
            }
            if self.debug && now - self.last_debug >= Duration::from_secs(5) {
                self.last_debug = now;
                let list: Vec<String> = self
                    .snap
                    .intervals
                    .iter()
                    .map(|iv| format!("{}@{}:[{},{}]", if iv.hwnd == 0 { "work".into() } else { format!("{:x}", iv.hwnd) }, iv.y, iv.x1, iv.x2))
                    .collect();
                eprintln!("[pet] pos=({:.0},{:.0}) surfaces={}", self.cx, self.fy, list.join(" "));
            }
        }

        // 活动范围和「移到显示器 N」
        let (scope, move_to, fs_switch) = {
            let mut s = self.shared.lock().unwrap();
            (s.screen_scope.clone(), s.move_to.take(), s.fs_switch)
        };
        self.scope = scope;
        if let Some(name) = move_to {
            if let Some(m) = self.snap.monitors.iter().find(|m| m.name == name).cloned() {
                let ratio = self.feet_monitor().map(|f| screens::x_ratio(f, self.cx)).unwrap_or(0.5);
                self.fs_origin = None;
                self.drop_onto(&m, ratio, k);
            }
        } else if self.scope.is_some() && !matches!(self.mode, Mode::Drag { .. }) {
            // 限定了显示器、桌宠却在别的屏上（刚改了范围）：挪过去
            let outside = self.feet_monitor().is_some_and(|m| !self.allowed(m));
            if outside {
                let ratio = self.feet_monitor().map(|f| screens::x_ratio(f, self.cx)).unwrap_or(0.5);
                if let Some(m) = self.allowed_monitors().first().map(|m| (*m).clone()) {
                    self.drop_onto(&m, ratio, k);
                }
            }
        }

        if now - self.last_fs_check >= FULLSCREEN_EVERY {
            self.last_fs_check = now;
            let fs = fullscreen::fullscreen_monitor(self.pid);
            let here = self.feet_monitor().map(|m| m.handle).unwrap_or(0);
            let mut fs_here = fs.covers(here);
            // 所在的屏全屏了：换到一块没在全屏的屏上（能去的话），不用隐藏
            if fs_here && fs_switch && !matches!(self.mode, Mode::Drag { .. }) {
                let target = self.allowed_monitors().into_iter().find(|m| !fs.covers(m.handle)).cloned();
                if let Some(t) = target {
                    if self.fs_origin.is_none() {
                        self.fs_origin = self.place();
                    }
                    let ratio = self.fs_origin.as_ref().map(|o| o.1).unwrap_or(0.5);
                    if self.debug {
                        eprintln!("[pet] 所在显示器全屏，换到 {}", t.name);
                    }
                    self.drop_onto(&t, ratio, k);
                    fs_here = false;
                }
            }
            // 原来那块屏不再全屏一段时间后，回去
            if let Some((name, ratio)) = self.fs_origin.clone() {
                let origin = self.snap.monitors.iter().find(|m| m.name == name).cloned();
                let busy = origin.as_ref().is_some_and(|m| fs.covers(m.handle));
                if busy && fs_switch {
                    self.fs_clear_since = None;
                } else {
                    let since = *self.fs_clear_since.get_or_insert(now);
                    if now - since >= FS_RETURN_AFTER || !fs_switch {
                        if let Some(m) = origin.filter(|m| self.allowed(m)) {
                            self.drop_onto(&m, ratio, k);
                        }
                        self.fs_origin = None;
                        self.fs_clear_since = None;
                    }
                }
            }
            if self.debug && fs_here != self.fs_now {
                eprintln!("[pet] fullscreen={fs_here} {fs:?} pet_mon={here:x} {}", fullscreen::describe_foreground());
            }
            self.fs_now = fs_here;
            self.fs_any = fs != FsMon::None;
        }
        self.fs_hidden = hide_fs && self.fs_now;
        // 只在启动时 show 一次；之后的隐藏是把窗口挪到屏幕外。
        // tao 之后的 show() 用的是 SW_SHOW，会把焦点从刚退出全屏的游戏那里抢走。
        if !self.shown {
            self.shown = true;
            let _ = self.window.show();
            // 状态标签窗口也只在这里 show 一次（先停在屏幕外），之后靠挪位置显示和隐藏
            if let Some(st) = self.app.get_webview_window(crate::windows::STATUS) {
                let vs = win::virtual_screen();
                let _ = st.set_position(PhysicalPosition::new(vs.r + 4000, vs.b + 4000));
                let _ = st.set_ignore_cursor_events(true);
                let _ = st.show();
                self.status = Some(st);
            }
        }
        let hidden = user_hidden || self.fs_hidden;
        {
            let mut s = self.shared.lock().unwrap();
            s.pet_hidden = hidden;
            // 因为全屏换了屏时，用户还在全屏里，主动聊天照样要安静
            s.fullscreen = self.fs_now || (self.fs_origin.is_some() && self.fs_any);
        }
        if hidden {
            self.press = None;
            if bubble.is_some() {
                crate::windows::close_bubble(&self.app);
            }
            self.follow_status(None);
            let vs = win::virtual_screen();
            let parked = (vs.r + 4000, vs.b + 4000);
            if self.last_pos != Some(parked) {
                self.last_pos = Some(parked);
                let _ = self.window.set_position(PhysicalPosition::new(parked.0, parked.1));
                let _ = self.window.set_ignore_cursor_events(true);
                self.ignore_cursor = Some(true);
            }
            return;
        }

        if reset {
            self.fs_origin = None;
            self.place_initial(k, false);
        }

        // 记下在哪块屏：退出时写进配置；变了的话最多 30 秒写一次，防止没正常退出时丢掉
        if now - self.last_place_check >= Duration::from_secs(2) && matches!(self.mode, Mode::Grounded { .. }) {
            self.last_place_check = now;
            // 因为全屏临时换了屏时，记的是原来那块
            if let Some(place) = self.fs_origin.clone().or_else(|| self.place()) {
                self.shared.lock().unwrap().place = Some(place.clone());
                let changed = self.saved_place.as_ref().is_none_or(|(n, x)| *n != place.0 || (x - place.1).abs() > 0.05);
                if changed && now - self.last_place_save >= PLACE_SAVE_EVERY {
                    self.last_place_save = now;
                    let state = self.app.state::<crate::AppState>();
                    let mut cfg = state.config.lock().unwrap();
                    cfg.pet_place = Some(crate::config::PetPlace { screen: place.0.clone(), x: place.1 });
                    crate::config::save(&cfg);
                    self.saved_place = Some(place);
                }
            }
        }

        // 区分单击和拖拽
        let cursor = win::cursor_pos();
        if pressed && !matches!(self.mode, Mode::Drag { .. }) {
            self.press = Some(cursor);
        }
        if let Some((px, py)) = self.press {
            if !win::primary_button_down() {
                self.press = None;
                self.on_click();
            } else if (cursor.0 - px).abs() > DRAG_THRESHOLD || (cursor.1 - py).abs() > DRAG_THRESHOLD {
                self.press = None;
                // 用户自己挪了，就不再「全屏结束后回原来那块屏」
                self.fs_origin = None;
                self.mode = Mode::Drag {
                    gx: px as f64 - self.cx,
                    gy: py as f64 - self.fy,
                    hist: VecDeque::new(),
                };
            }
        }

        let mode = std::mem::replace(&mut self.mode, Mode::Air { vx: 0.0, vy: 0.0 });
        self.mode = match mode {
            Mode::Drag { gx, gy, hist } => self.step_drag(gx, gy, hist, cursor, k, h),
            Mode::Air { vx, vy } => self.step_air(vx, vy, dt, k, w, h),
            // 对话中或气泡开着时也不乱跑，免得气泡追着跑
            Mode::Grounded { ground, act, until } => {
                let quiet = work_mode || speech.is_some() || bubble.is_some() || agent_anim.is_some();
                self.step_ground(ground, act, until, quiet, dt, k, w, h)
            }
        };

        let left = (self.cx - metrics.center_ratio * w).round() as i32;
        let top = (self.fy - metrics.foot_ratio * h).round() as i32;
        if self.last_pos != Some((left, top)) {
            self.last_pos = Some((left, top));
            let _ = self.window.set_position(PhysicalPosition::new(left, top));
        }

        match bubble {
            Some((size, moved)) => self.follow_bubble(left, top, w, h, &metrics, size, moved),
            // 收起后忘掉位置，下次打开时重新记一行日志
            None => self.bubble_pos = None,
        }
        let status_size = self.shared.lock().unwrap().status_size;
        let place = (status_size.0 > 0).then(|| {
            let head_y = top as f64 + metrics.top_ratio * h;
            // 身体半宽约为格子宽度的 0.2；拖拽中也照样跟着
            (head_y, self.fy, w * 0.2, status_size)
        });
        self.follow_status(place);
        if now - self.last_z_check >= ZORDER_EVERY {
            self.last_z_check = now;
            self.keep_on_top(bubble.is_some(), status_size.0 > 0);
        }

        // 朝向跟着水平移动走，停下时保持。先于动画发出，换动画的第一帧朝向就是对的
        let moving = match &self.mode {
            Mode::Air { vx, .. } if vx.abs() > 1e-3 => Some(vx.signum()),
            Mode::Grounded { act: Act::Walk { dir }, .. } => Some(dir.signum()),
            _ => None,
        };
        if let Some(d) = moving.filter(|d| *d != self.facing) {
            self.facing = d;
            let _ = self.app.emit_to("pet", "pet-facing", if d > 0.0 { "right" } else { "left" });
        }

        let anim = match &self.mode {
            // 优先级：自己的对话 > agent 状态 > 自主行为
            Mode::Grounded { act, .. } if speech.is_some() && !matches!(act, Act::Wave | Act::Land) => speech.unwrap(),
            Mode::Grounded { act, .. } if agent_anim.is_some() && !matches!(act, Act::Wave | Act::Land) => agent_anim.unwrap(),
            Mode::Drag { .. } => "drag",
            Mode::Air { vy, .. } if *vy < 0.0 => "jump",
            Mode::Air { .. } => "fall",
            Mode::Grounded { act, .. } => match act {
                Act::Idle => "idle",
                Act::Walk { dir } if *dir > 0.0 => "walk_right",
                Act::Walk { .. } => "walk_left",
                Act::Sleep => "sleep",
                Act::Sit => "sit",
                Act::Wave => "wave",
                Act::Land => "land",
            },
        };
        if anim != self.anim {
            if self.debug {
                eprintln!("[pet] {} -> {} at ({:.0},{:.0})", self.anim, anim, self.cx, self.fy);
            }
            self.anim = anim;
            let _ = self.app.emit_to("pet", "pet-anim", anim);
        }

        let interactive = matches!(self.mode, Mode::Drag { .. }) || self.press.is_some() || {
            let s = self.shared.lock().unwrap();
            hittest::hit(&s.masks, s.frame, rect, cursor)
        };
        if self.ignore_cursor != Some(!interactive) {
            self.ignore_cursor = Some(!interactive);
            let _ = self.window.set_ignore_cursor_events(!interactive);
        }
    }

    /// 桌宠的窗口都是置顶的，但别的置顶窗口（置顶的播放器、截图贴图、别的桌宠等）后激活就会盖在上面；
    /// 气泡和状态标签一直是隐藏后再显示，层级还停在上次的位置，更容易被盖住。
    /// 被盖住时按 桌宠 → 状态标签 → 气泡 的顺序提回最上面（气泡在最上面）。用户按着鼠标时不动，免得打断别的程序的操作
    fn keep_on_top(&mut self, bubble: bool, status: bool) {
        if win::primary_button_down() {
            return;
        }
        let handle = |label: &str| self.app.get_webview_window(label).and_then(|w| w.hwnd().ok()).map(|h| h.0 as isize).unwrap_or(0);
        let mut targets = vec![self.hwnd];
        if status {
            targets.push(handle(crate::windows::STATUS));
        }
        if bubble {
            targets.push(handle(crate::windows::BUBBLE));
        }
        targets.retain(|&h| h != 0);
        let Some(cover) = targets.iter().find_map(|&t| win::covering_window(t, self.pid)) else {
            return;
        };
        let who = if cover == 0 {
            "（自己丢了置顶）".to_string()
        } else {
            format!("{} {}", win::process_name(win::window_pid(cover)).unwrap_or_default(), win::class_name(cover))
        };
        if who != self.last_cover {
            eprintln!("[pet] 被 {who} 盖住，提回最上面");
            self.last_cover = who;
        }
        for t in targets {
            win::raise_topmost(t);
        }
    }

    fn step_drag(
        &mut self,
        gx: f64,
        gy: f64,
        mut hist: VecDeque<(Instant, f64, f64)>,
        cursor: (i32, i32),
        k: f64,
        h: f64,
    ) -> Mode {
        let now = Instant::now();
        let (x, y) = (cursor.0 as f64, cursor.1 as f64);
        self.cx = x - gx;
        self.fy = y - gy;
        hist.push_back((now, x, y));
        while hist.len() > 2 && now - hist[0].0 > Duration::from_millis(80) {
            hist.pop_front();
        }
        if win::primary_button_down() {
            return Mode::Drag { gx, gy, hist };
        }
        let (vx, vy) = throw_velocity(&hist);
        let (vx, vy) = (vx.clamp(-MAX_THROW * k, MAX_THROW * k), vy.clamp(-MAX_THROW * k, MAX_THROW * k));
        if vx.hypot(vy) < 400.0 * k {
            if let Some(iv) = self.drop_target(h) {
                self.fy = iv.y;
                return self.grounded(&iv, Act::Land, Duration::from_millis(350));
            }
        }
        Mode::Air { vx, vy }
    }

    fn step_air(&mut self, mut vx: f64, mut vy: f64, dt: f64, k: f64, w: f64, h: f64) -> Mode {
        let prev = self.fy;
        vy += GRAVITY * k * dt;
        self.cx += vx * dt;
        self.fy += vy * dt;

        // 只能待在真实存在的（能去的）屏幕区域里：几块屏大小不一时中间的空隙、限定显示器时别的屏，都当墙
        let half = w * 0.5;
        let body = self.fy - h * 0.5;
        let allowed: Vec<Monitor> = self.allowed_monitors().into_iter().cloned().collect();
        let inside = |x: f64, y: f64| allowed.iter().any(|m| screens::contains(m, x, y));
        if !inside(self.cx, body) {
            if let Some(m) = screens::at_or_nearest(&allowed, self.cx, body) {
                let (x, y, hit_x, hit_y) = screens::clamp_into(m, self.cx, self.fy, half, h);
                if hit_x {
                    vx = if x > self.cx { vx.abs() } else { -vx.abs() } * 0.5;
                }
                if hit_y && y > self.fy {
                    vy = vy.max(0.0); // 顶到头
                }
                self.cx = x;
                self.fy = y;
            }
        } else if let Some(m) = allowed.iter().find(|m| screens::contains(m, self.cx, body)) {
            // 身体半边伸出屏幕、旁边又没有能去的屏：弹回来
            if !inside(self.cx - half, body) && self.cx - half < m.full.l as f64 {
                self.cx = m.full.l as f64 + half;
                vx = vx.abs() * 0.5;
            }
            if !inside(self.cx + half, body) && self.cx + half > m.full.r as f64 {
                self.cx = m.full.r as f64 - half;
                vx = -vx.abs() * 0.5;
            }
            if !inside(self.cx, self.fy - h) && self.fy - h < m.full.t as f64 {
                self.fy = m.full.t as f64 + h;
                vy = vy.max(0.0);
            }
        }

        if vy > 0.0 {
            if let Some(iv) = self.landing(prev).or_else(|| self.fallback_landing()) {
                self.fy = iv.y;
                return self.grounded(&iv, Act::Land, Duration::from_millis(350));
            }
        }
        Mode::Air { vx, vy }
    }

    #[allow(clippy::too_many_arguments)]
    fn step_ground(
        &mut self,
        ground: Ground,
        mut act: Act,
        mut until: Instant,
        work_mode: bool,
        dt: f64,
        k: f64,
        w: f64,
        h: f64,
    ) -> Mode {
        let now = Instant::now();
        let fall = Mode::Air { vx: 0.0, vy: 0.0 };
        let can_sit = self.shared.lock().unwrap().can_sit;

        // 先跟随地面：任务栏高度可能变，宿主窗口可能移动、消失或被遮挡
        let (ground, bounds) = match ground {
            Ground::Work => match self.work_bounds() {
                Some((l, r, b)) => {
                    self.fy = b;
                    (Ground::Work, (l, r))
                }
                None => return fall,
            },
            Ground::Window { hwnd, rect, mut changed_at } => {
                let Some(r) = surfaces::host_rect(hwnd) else { return fall };
                if r != rect {
                    self.cx += (r.l - rect.l) as f64;
                    changed_at = now;
                }
                self.fy = r.t as f64;
                let settled = now - changed_at > HOST_SETTLE && self.snap.at > changed_at;
                let bounds = match self.host_interval(hwnd, r) {
                    Some(b) => b,
                    None if settled => return fall,
                    None => (r.l as f64, r.r as f64),
                };
                (Ground::Window { hwnd, rect: r, changed_at }, bounds)
            }
        };

        if work_mode && matches!(act, Act::Walk { .. } | Act::Sleep) {
            act = Act::Idle;
        }
        if now >= until {
            if work_mode {
                act = Act::Idle;
                until = now + Duration::from_secs(1);
            } else {
                // 站在窗口顶边、皮肤会坐时，可以坐一会儿（调试：PERCH_DEBUG_SIT=1 时一有机会就坐）
                let may_sit = can_sit && matches!(ground, Ground::Window { .. });
                let r = if may_sit && self.force_sit { 0.7 } else { fastrand::f64() };
                if r < 0.34 {
                    act = Act::Walk { dir: if fastrand::bool() { 1.0 } else { -1.0 } };
                    until = now + rand_secs(2.0, 6.0);
                } else if r < 0.54 {
                    if let Some((vx, vy)) = self.plan_jump(k, w, h) {
                        return Mode::Air { vx, vy };
                    }
                    act = Act::Idle;
                    until = now + rand_secs(2.0, 5.0);
                } else if r < 0.62 {
                    act = Act::Sleep;
                    until = now + rand_secs(10.0, 25.0);
                } else if r < 0.82 && may_sit {
                    act = Act::Sit;
                    until = now + rand_secs(20.0, 60.0);
                } else {
                    act = Act::Idle;
                    until = now + rand_secs(3.0, 8.0);
                }
            }
        }

        if let Act::Walk { dir } = &mut act {
            let margin = w * 0.3;
            let (lo, hi) = (bounds.0 + margin, bounds.1 - margin);
            if hi <= lo {
                act = Act::Idle;
            } else {
                self.cx += *dir * WALK_SPEED * k * dt;
                if self.cx < lo {
                    self.cx = lo;
                    *dir = 1.0;
                } else if self.cx > hi {
                    self.cx = hi;
                    *dir = -1.0;
                }
            }
        }

        Mode::Grounded { ground, act, until }
    }
}

pub fn spawn(app: AppHandle, shared: SharedState) {
    std::thread::spawn(move || {
        let window = loop {
            if let Some(w) = app.get_webview_window("pet") {
                break w;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let hwnd = window.hwnd().map(|h| h.0 as isize).unwrap_or(0);
        let mut ctl = Ctl::new(app, window, hwnd, shared);
        let mut last = Instant::now();
        loop {
            let now = Instant::now();
            let dt = (now - last).as_secs_f64().min(0.05);
            last = now;
            ctl.tick(dt);
            std::thread::sleep(TICK);
        }
    });
}

/// 前台窗口若不属于本进程，返回 (hwnd, pid, process_name)
fn foreground_non_self(self_pid: u32) -> Option<(isize, u32, String)> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    let fg = unsafe { GetForegroundWindow() }.0 as isize;
    if fg == 0 {
        return None;
    }
    let pid = win::window_pid(fg);
    if pid == 0 || pid == self_pid {
        return None;
    }
    let name = win::process_name(pid)?;
    Some((fg, pid, name))
}
