//! 按当前帧的 alpha 掩码判断光标是否落在角色身上，决定窗口要不要鼠标穿透。

use super::win::Rect;

/// 前端算好的每帧掩码：mask_w × mask_h 的位图，按行优先打包成字节
#[derive(Clone, Debug, Default)]
pub struct Masks {
    pub w: usize,
    pub h: usize,
    pub frames: Vec<Vec<u8>>,
}

impl Masks {
    /// 单帧的运行时掩码，尺寸和长度不对时拒绝
    pub fn live(w: usize, h: usize, bits: Vec<u8>) -> Result<Self, String> {
        if w == 0 || h == 0 || w > 1024 || h > 1024 {
            return Err(format!("掩码尺寸不对: {w}×{h}"));
        }
        if bits.len() < (w * h).div_ceil(8) {
            return Err("掩码数据长度不够".into());
        }
        Ok(Self { w, h, frames: vec![bits] })
    }

    fn bit(&self, frame: usize, x: usize, y: usize) -> bool {
        let Some(bits) = self.frames.get(frame) else { return false };
        let i = y * self.w + x;
        bits.get(i / 8).is_some_and(|b| b & (1 << (i % 8)) != 0)
    }
}

pub fn hit(masks: &Masks, frame: usize, win: Rect, cursor: (i32, i32)) -> bool {
    if masks.w == 0 || masks.h == 0 || win.w() <= 0 || win.h() <= 0 {
        // 掩码还没到时整个窗口都可点，避免点不到
        return true;
    }
    let (cx, cy) = cursor;
    if cx < win.l || cx >= win.r || cy < win.t || cy >= win.b {
        return false;
    }
    let x = ((cx - win.l) as usize * masks.w) / win.w() as usize;
    let y = ((cy - win.t) as usize * masks.h) / win.h() as usize;
    // 放宽一格，细小部位也好点中
    let (x0, x1) = (x.saturating_sub(1), (x + 1).min(masks.w - 1));
    let (y0, y1) = (y.saturating_sub(1), (y + 1).min(masks.h - 1));
    (y0..=y1).any(|yy| (x0..=x1).any(|xx| masks.bit(frame, xx, yy)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_follows_mask() {
        // 4×4 掩码，只有右下角 (3,3) 不透明
        let mut bits = vec![0u8; 2];
        bits[15 / 8] |= 1 << (15 % 8);
        let m = Masks { w: 4, h: 4, frames: vec![bits] };
        let win = Rect { l: 100, t: 100, r: 140, b: 140 };
        assert!(hit(&m, 0, win, (138, 138)));
        assert!(!hit(&m, 0, win, (101, 101)));
        assert!(!hit(&m, 0, win, (50, 50)));
    }

    #[test]
    fn live_mask_is_checked() {
        assert!(Masks::live(0, 4, vec![0; 2]).is_err());
        assert!(Masks::live(4, 4, vec![0; 1]).is_err());
        assert!(Masks::live(2000, 4, vec![0; 1000]).is_err());
        let mut bits = vec![0u8; 2];
        bits[0] |= 1;
        let m = Masks::live(4, 4, bits).unwrap();
        let win = Rect { l: 0, t: 0, r: 40, b: 40 };
        assert!(hit(&m, 0, win, (2, 2)));
        assert!(!hit(&m, 0, win, (35, 35)));
    }
}
