//! 桌面感知：廉价信号（一期）、按需看屏（二期）、变化打分（三期）。

pub mod capture;
pub mod cuts;
pub mod gate;
pub mod signals;

pub use capture::{
    capture, downscale_area_average, encode_png_rgba, is_seeing, note_foreground, set_seeing,
    target_record, Scope, Shot,
};
pub use cuts::{hsv_grid_from_rgba, is_significant, score_pair, SIGNIFICANT_CHANGE_SCORE};
pub use gate::{gate, CaptureDeny, WindowInfo};
pub use signals::{signals, Signals};
