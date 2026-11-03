//! 桌面感知：廉价信号（一期）、按需看屏（二期）、变化打分（三期）。

pub mod capture;
pub mod cuts;
pub mod gate;
pub mod signals;

pub use capture::{capture, is_seeing, note_foreground, set_seeing, Scope};
pub use cuts::{is_significant, score_pair};
pub use gate::gate;
pub use signals::signals;
