//! 桌面感知：廉价信号（一期）与按需看屏（二期）。

pub mod capture;
pub mod gate;
pub mod signals;

pub use capture::{capture, is_seeing, note_foreground, set_seeing, Scope};
pub use signals::signals;
