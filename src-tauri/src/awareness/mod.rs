//! 桌面感知：廉价信号（一期）与按需看屏（二期）。

pub mod capture;
pub mod gate;
pub mod signals;

pub use capture::{
    capture, downscale_area_average, encode_png_rgba, is_seeing, note_foreground, set_seeing,
    target_record, Scope, Shot,
};
pub use gate::{gate, CaptureDeny, WindowInfo};
pub use signals::{signals, Signals};
