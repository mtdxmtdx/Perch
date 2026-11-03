//! 画面变化打分：移植自 dsh-see-now 的 `src/cuts.js`（`gridRgb` / `rgbToHsv` / `scorePair`）。
//!
//! 上游：https://github.com/SAOS-3624/dsh-see-now
//! 文件：`src/cuts.js`
//! 提交：`9bb7df44ee6e7b9d52c34373092e79fd90850641`（release 0.0.5）
//! 许可：Apache-2.0
//! **已修改**：由 JavaScript 移植为 Rust；函数名改为 snake_case；增加 RGBA 入口与显著变化阈值常量。
//!
//! `scorePair` 原始算法亦见于同一作者的 dsh-video-storyboard（`scan.js`，Apache-2.0），一并署名。
//! 完整 NOTICE / Apache-2.0 文本见仓库根目录 `THIRD_PARTY_NOTICES.md`。

#![allow(clippy::needless_range_loop)]

pub const SCAN_W: usize = 64;
pub const SCAN_H: usize = 36;

/// 与 dsh-see-now 默认 `pace: normal` 的 `cutAbs` 对齐，用作「画面显著变化」阈值（0–255 分）。
pub const SIGNIFICANT_CHANGE_SCORE: f32 = 12.0;

/// 64×36 网格：每格取 4 个采样点求平均。`rgb` 为紧密 RGB 交错（每像素 3 通道，0–255）。
pub fn grid_rgb(rgb: &[f32], width: u32, height: u32, out: &mut [f32]) {
    assert_eq!(out.len(), SCAN_W * SCAN_H * 3);
    let sx = width as f32 / SCAN_W as f32;
    let sy = height as f32 / SCAN_H as f32;
    for gy in 0..SCAN_H {
        let y0 = ((gy as f32 + 0.25) * sy) as u32 as usize;
        let y1 = ((gy as f32 + 0.75) * sy) as u32 as usize;
        let r0 = y0 * width as usize;
        let r1 = y1 * width as usize;
        for gx in 0..SCAN_W {
            let x0 = ((gx as f32 + 0.25) * sx) as u32 as usize;
            let x1 = ((gx as f32 + 0.75) * sx) as u32 as usize;
            let a = (r0 + x0) * 3;
            let b = (r0 + x1) * 3;
            let c = (r1 + x0) * 3;
            let d = (r1 + x1) * 3;
            let i = gy * SCAN_W + gx;
            out[i * 3] = (rgb[a] + rgb[b] + rgb[c] + rgb[d]) * 0.25;
            out[i * 3 + 1] = (rgb[a + 1] + rgb[b + 1] + rgb[c + 1] + rgb[d + 1]) * 0.25;
            out[i * 3 + 2] = (rgb[a + 2] + rgb[b + 2] + rgb[c + 2] + rgb[d + 2]) * 0.25;
        }
    }
}

/// RGB → HSV（各通道归一到 0–255，H 由角度缩放）。就地写进 `out`。
pub fn rgb_to_hsv(rgb: &[f32], out: &mut [f32]) {
    assert_eq!(rgb.len(), out.len());
    let mut i = 0;
    while i < rgb.len() {
        let rn = rgb[i] / 255.0;
        let gn = rgb[i + 1] / 255.0;
        let bn = rgb[i + 2] / 255.0;
        let max = rn.max(gn).max(bn);
        let min = rn.min(gn).min(bn);
        let d = max - min;
        let mut h = 0.0f32;
        if d != 0.0 {
            if (max - rn).abs() < f32::EPSILON {
                h = ((gn - bn) / d) % 6.0;
            } else if (max - gn).abs() < f32::EPSILON {
                h = (bn - rn) / d + 2.0;
            } else {
                h = (rn - gn) / d + 4.0;
            }
            h *= 60.0;
            if h < 0.0 {
                h += 360.0;
            }
        }
        out[i] = (h / 360.0) * 255.0;
        out[i + 1] = if max == 0.0 { 0.0 } else { (d / max) * 255.0 };
        out[i + 2] = max * 255.0;
        i += 3;
    }
}

/// 两帧 HSV 的加权绝对差均值，0–255。色相按环形处理，低饱和像素的色相降权。
pub fn score_pair(prev: &[f32], cur: &[f32]) -> f32 {
    assert_eq!(prev.len(), cur.len());
    assert!(!cur.is_empty());
    let mut sum = 0.0f32;
    let mut i = 0;
    while i < cur.len() {
        let dh = (cur[i] - prev[i]).abs();
        let hue_diff = dh.min(255.0 - dh);
        let hue_weight = cur[i + 1].min(prev[i + 1]) / 255.0;
        sum += hue_diff * hue_weight + (cur[i + 1] - prev[i + 1]).abs() + (cur[i + 2] - prev[i + 2]).abs();
        i += 3;
    }
    sum / ((cur.len() as f32 / 3.0) * 3.0)
}

/// 从 RGBA8 生成 64×36 HSV 网格（先按面积采样到网格）。
pub fn hsv_grid_from_rgba(rgba: &[u8], width: u32, height: u32) -> Vec<f32> {
    let n = (width as usize) * (height as usize);
    assert_eq!(rgba.len(), n * 4);
    let mut rgb = Vec::with_capacity(n * 3);
    for px in rgba.chunks_exact(4) {
        rgb.push(px[0] as f32);
        rgb.push(px[1] as f32);
        rgb.push(px[2] as f32);
    }
    let mut grid = vec![0.0f32; SCAN_W * SCAN_H * 3];
    grid_rgb(&rgb, width, height, &mut grid);
    let mut hsv = vec![0.0f32; grid.len()];
    rgb_to_hsv(&grid, &mut hsv);
    hsv
}

pub fn is_significant(score: f32) -> bool {
    score >= SIGNIFICANT_CHANGE_SCORE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_rgb(w: u32, h: u32, r: f32, g: f32, b: f32) -> Vec<f32> {
        let n = (w * h) as usize;
        let mut v = Vec::with_capacity(n * 3);
        for _ in 0..n {
            v.push(r);
            v.push(g);
            v.push(b);
        }
        v
    }

    fn checker_rgb(w: u32, h: u32) -> Vec<f32> {
        let mut v = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                let on = ((x >> 3) ^ (y >> 3)) & 1 == 1;
                v.push(if on { 255.0 } else { 0.0 });
                v.push(if on { 0.0 } else { 255.0 });
                v.push(if on { 128.0 } else { 64.0 });
            }
        }
        v
    }

    fn hsv_of(rgb: &[f32], w: u32, h: u32) -> Vec<f32> {
        let mut grid = vec![0.0f32; SCAN_W * SCAN_H * 3];
        grid_rgb(rgb, w, h, &mut grid);
        let mut hsv = vec![0.0f32; grid.len()];
        rgb_to_hsv(&grid, &mut hsv);
        hsv
    }

    /// 与上游提交 9bb7df4 用 node 跑同一合成输入得到的参考值对拍
    #[test]
    fn matches_upstream_js_reference() {
        let w = 128u32;
        let h = 72u32;
        let red = hsv_of(&solid_rgb(w, h, 255.0, 0.0, 0.0), w, h);
        let blue = hsv_of(&solid_rgb(w, h, 0.0, 0.0, 255.0), w, h);
        let score_rb = score_pair(&red, &blue);
        assert!((score_rb - 28.333333333333332).abs() < 1e-4, "score_rb={score_rb}");

        let checker = hsv_of(&checker_rgb(w, h), w, h);
        let dark = hsv_of(&solid_rgb(w, h, 10.0, 10.0, 10.0), w, h);
        let score_cd = score_pair(&checker, &dark);
        assert!((score_cd - 166.66666666666666).abs() < 1e-3, "score_cd={score_cd}");

        assert_eq!(score_pair(&red, &red), 0.0);

        let rgb_tiny = [255.0, 0.0, 0.0, 0.0, 255.0, 0.0, 0.0, 0.0, 255.0, 128.0, 128.0, 128.0];
        let mut hsv_tiny = [0.0f32; 12];
        rgb_to_hsv(&rgb_tiny, &mut hsv_tiny);
        let expect = [0.0, 255.0, 255.0, 85.0, 255.0, 255.0, 170.0, 255.0, 255.0, 0.0, 0.0, 128.0];
        for (a, b) in hsv_tiny.iter().zip(expect.iter()) {
            assert!((a - b).abs() < 1e-4, "{a} vs {b}");
        }

        assert!(is_significant(score_rb));
        assert!(!is_significant(0.0));
        assert!(!is_significant(11.9));
        assert!(is_significant(12.0));
    }

    /// 性能抽样（耗时随机器浮动，默认跳过）：cargo test --release -- --ignored perf_sample
    #[test]
    #[ignore]
    fn perf_sample_score_pair() {
        let w = 128u32;
        let h = 72u32;
        let a = hsv_of(&solid_rgb(w, h, 255.0, 0.0, 0.0), w, h);
        let b = hsv_of(&solid_rgb(w, h, 0.0, 0.0, 255.0), w, h);
        let t0 = std::time::Instant::now();
        let mut last = 0.0f32;
        for _ in 0..200 {
            last = score_pair(&a, &b);
        }
        let elapsed = t0.elapsed();
        let per = elapsed / 200;
        eprintln!("[perf] score_pair ×200 = {elapsed:?} (≈{per:?}/call) last={last}");
        assert!(per.as_millis() < 50, "score_pair too slow: {per:?}");
        assert!(last > 0.0);
    }
}
