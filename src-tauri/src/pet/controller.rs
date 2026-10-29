//! 桌宠控制线程（约 60Hz）：物理、自主行为、拖拽、鼠标穿透、全屏隐藏。
//! 位置用"身体中心 x + 脚底 y"（物理像素）表示，每帧换算成窗口左上角。

use super::surfaces::{self, Interval, Snapshot};
use super::win::{self, Rect};
use super::{fullscreen, hittest, SharedState};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};

const TICK: Duration = Duration::from_millis(16);
const SNAPSHOT_EVERY: Duration = Duration::from_millis(100);
const FULLSCREEN_EVERY: Duration = Duration::from_millis(400);
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
    last_fs_check: Instant,
    shown: bool,
    anim: &'static str,
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
            last_fs_check: Instant::now(),
            shown: false,
            anim: "",
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
        ctl.place_initial(1.0);
        ctl
    }

    /// 从主显示器中间偏下的位置掉下来
    fn place_initial(&mut self, k: f64) {
        let mons = &self.snap.monitors;
        let m = mons
            .iter()
            .find(|m| m.full.l <= 0 && m.full.r > 0 && m.full.t <= 0 && m.full.b > 0)
            .or(mons.first());
        if let Some(m) = m {
            self.cx = (m.work.l + m.work.r) as f64 / 2.0;
            self.fy = m.work.b as f64 - 260.0 * k;
        }
        self.mode = Mode::Air { vx: 0.0, vy: 0.0 };
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
        self.snap
            .monitors
            .iter()
            .find(|m| m.work.contains_x(self.cx))
            .map(|m| (m.work.l as f64, m.work.r as f64, m.work.b as f64))
    }

    /// 本帧下落时穿过的最高可站面
    fn landing(&self, prev_fy: f64) -> Option<Interval> {
        self.snap
            .intervals
            .iter()
            .filter(|iv| prev_fy <= iv.y + 0.5 && self.fy >= iv.y && self.cx >= iv.x1 && self.cx <= iv.x2)
            .min_by(|a, b| a.y.total_cmp(&b.y))
            .copied()
    }

    /// 掉出所有可站面时（比如显示器之间的缝隙），落到最近显示器的任务栏上
    fn fallback_landing(&mut self) -> Option<Interval> {
        let m = self
            .snap
            .monitors
            .iter()
            .min_by_key(|m| {
                let d = if self.cx < m.work.l as f64 {
                    m.work.l as f64 - self.cx
                } else if self.cx > m.work.r as f64 {
                    self.cx - m.work.r as f64
                } else {
                    0.0
                };
                d as i64
            })?
            .clone();
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
            .min_by(|a, b| (self.fy - a.y).total_cmp(&(self.fy - b.y)))
            .copied()
    }

    /// 随机挑一个够得着的面，算出起跳速度
    fn plan_jump(&self, k: f64, w: f64, h: f64) -> Option<(f64, f64)> {
        let margin = w * 0.35;
        // 头不能顶出屏幕：最高点的脚底至少比屏幕顶低一个身高
        let ceiling = win::virtual_screen().t as f64 + h + 4.0;
        let cands: Vec<(f64, f64)> = self
            .snap
            .intervals
            .iter()
            .filter_map(|iv| {
                if iv.x2 - iv.x1 < 2.0 * margin + 10.0 {
                    return None;
                }
                if (iv.y - self.fy).abs() < 2.0 && self.cx >= iv.x1 && self.cx <= iv.x2 {
                    return None;
                }
                let tx = iv.x1 + margin + fastrand::f64() * (iv.x2 - iv.x1 - 2.0 * margin);
                let dx = (tx - self.cx).abs();
                let rise = self.fy - iv.y;
                let ok = iv.y - ceiling >= 20.0 * k
                    && dx >= 60.0 * k && dx <= MAX_JUMP_DX * k && rise <= MAX_JUMP_RISE * k && rise >= -1600.0 * k;
                ok.then_some((tx, iv.y))
            })
            .collect();
        if cands.is_empty() {
            return None;
        }
        let (tx, ty) = cands[fastrand::usize(..cands.len())];
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
            .snap
            .monitors
            .iter()
            .find(|mon| mon.full.contains_x(self.cx))
            .or(self.snap.monitors.first())
            .map(|mon| (mon.work.l, mon.work.t, mon.work.r, mon.work.b))
            .unwrap_or((0, 0, 1920, 1080));
        let (pos, placement) =
            crate::windows::place_bubble((left, top, w as i32, h as i32), self.cx, head_y, size, work);
        if moved || self.bubble_pos != Some(pos) {
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

        if !hide_fs {
            self.fs_hidden = false;
        } else if now - self.last_fs_check >= FULLSCREEN_EVERY {
            self.last_fs_check = now;
            let fs = fullscreen::is_fullscreen_at(self.cx as i32, self.fy as i32 - 1, self.pid);
            if self.debug && fs != self.fs_hidden {
                eprintln!("[pet] fullscreen={fs} pet_mon={:x} {}", win::monitor_at_point(self.cx as i32, self.fy as i32 - 1), fullscreen::describe_foreground());
            }
            self.fs_hidden = fs;
        }
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
        self.shared.lock().unwrap().pet_hidden = hidden;
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
            self.place_initial(k);
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

        if let Some((size, moved)) = bubble {
            self.follow_bubble(left, top, w, h, &metrics, size, moved);
        }
        let status_size = self.shared.lock().unwrap().status_size;
        let place = (status_size.0 > 0).then(|| {
            let head_y = top as f64 + metrics.top_ratio * h;
            // 身体半宽约为格子宽度的 0.2；拖拽中也照样跟着
            (head_y, self.fy, w * 0.2, status_size)
        });
        self.follow_status(place);

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

        let vs = win::virtual_screen();
        let half = w * 0.5;
        if self.cx - half < vs.l as f64 {
            self.cx = vs.l as f64 + half;
            vx = vx.abs() * 0.5;
        }
        if self.cx + half > vs.r as f64 {
            self.cx = vs.r as f64 - half;
            vx = -vx.abs() * 0.5;
        }
        if self.fy - h < vs.t as f64 {
            self.fy = vs.t as f64 + h;
            vy = vy.max(0.0);
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
