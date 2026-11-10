//! アイコンの形と色。トレイアイコン（実行時）と exe のアイコン（build.rs）の両方で使うため、
//! 標準ライブラリ以外に依存しない。

pub const GREEN: [u8; 3] = [0x22, 0xC5, 0x5E];
pub const GRAY: [u8; 3] = [0x94, 0xA3, 0xB8];
pub const RED: [u8; 3] = [0xEF, 0x44, 0x44];

/// 残量の塗りの段階数。16px でも 1 段が 2px 以上になり見分けられる数にする
pub const STEPS: u8 = 5;

#[derive(Clone, Copy)]
pub enum Shape {
    Disc,
    Ring,
}

/// 片側のイヤホンのバッテリーの見た目
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gauge {
    /// 残量が分からない（ケースに戻した側など）。空の枠で描く
    Empty,
    /// `steps` は 0–`STEPS`。`low` なら赤で描く
    Level { steps: u8, low: bool },
}

/// 上から下への BGRA。縁は 4x4 のサンプリングでなめらかにする
pub fn pixels(size: usize, shape: Shape, [r, g, b]: [u8; 3]) -> Vec<u8> {
    let c = size as f32 / 2.0;
    let outer = size as f32 * 0.44;
    let inner = match shape {
        Shape::Disc => 0.0,
        Shape::Ring => size as f32 * 0.26,
    };
    let mut out = Vec::with_capacity(size * size * 4);
    for y in 0..size {
        for x in 0..size {
            let mut hits = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let dx = x as f32 + (sx as f32 + 0.5) / 4.0 - c;
                    let dy = y as f32 + (sy as f32 + 0.5) / 4.0 - c;
                    let d = (dx * dx + dy * dy).sqrt();
                    if d <= outer && d >= inner {
                        hits += 1;
                    }
                }
            }
            out.extend_from_slice(&[b, g, r, (hits * 255 / 16) as u8]);
        }
    }
    out
}

/// 左右 2 本の縦長のバッテリー。上から下への BGRA。
/// 線がにじまないよう、すべて整数のピクセルにそろえた長方形で描く
pub fn batteries(size: usize, gauges: [Gauge; 2]) -> Vec<u8> {
    let unit = (size + 8) / 16;
    let stroke = (size / 16).max(1);
    let width = (size * 6 + 8) / 16;
    let gap = size - 2 * width - 2 * unit;
    let cap_inset = width / 3;
    let body_top = unit + stroke;
    let body_bottom = size - unit;
    // 枠と塗りの間のすき間。16px と 20px では塗りが細くなりすぎるので空けない
    let pad = size / 24;
    let inner_top = body_top + stroke + pad;
    let inner_bottom = body_bottom - stroke - pad;
    let inner_height = inner_bottom - inner_top;

    let mut out = vec![0u8; size * size * 4];
    let mut rect = |x0: usize, y0: usize, x1: usize, y1: usize, [r, g, b]: [u8; 3]| {
        for y in y0..y1 {
            for x in x0..x1 {
                out[(y * size + x) * 4..][..4].copy_from_slice(&[b, g, r, 0xFF]);
            }
        }
    };
    for (i, gauge) in gauges.into_iter().enumerate() {
        let left = unit + i * (width + gap);
        let right = left + width;
        let (rgb, steps) = match gauge {
            Gauge::Empty => (GRAY, 0),
            Gauge::Level { steps, low } => (if low { RED } else { GREEN }, steps.min(STEPS) as usize),
        };
        rect(left + cap_inset, unit, right - cap_inset, body_top, rgb);
        rect(left, body_top, right, body_top + stroke, rgb);
        rect(left, body_bottom - stroke, right, body_bottom, rgb);
        rect(left, body_top, left + stroke, body_bottom, rgb);
        rect(right - stroke, body_top, right, body_bottom, rgb);
        let fill = (inner_height * steps + STEPS as usize / 2) / STEPS as usize;
        rect(left + stroke + pad, inner_bottom - fill, right - stroke - pad, inner_bottom, rgb);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 不透明なピクセルの数
    fn opaque(px: &[u8]) -> usize {
        px.chunks(4).filter(|p| p[3] == 0xFF).count()
    }

    #[test]
    fn batteries_fit_every_icon_size() {
        for size in [16, 20, 24, 32, 48] {
            let px = batteries(size, [Gauge::Level { steps: STEPS, low: false }; 2]);
            assert_eq!(px.len(), size * size * 4);
        }
    }

    #[test]
    fn each_step_fills_more() {
        for size in [16, 20, 24, 32] {
            let counts: Vec<usize> = (0..=STEPS)
                .map(|steps| opaque(&batteries(size, [Gauge::Level { steps, low: false }; 2])))
                .collect();
            assert!(counts.windows(2).all(|w| w[0] < w[1]), "{size}px: {counts:?}");
        }
    }

    #[test]
    fn empty_frame_matches_zero_steps_in_gray() {
        let empty = batteries(16, [Gauge::Empty; 2]);
        let zero = batteries(16, [Gauge::Level { steps: 0, low: false }; 2]);
        assert_eq!(opaque(&empty), opaque(&zero));
        let colors: Vec<&[u8]> = empty.chunks(4).filter(|p| p[3] == 0xFF).collect();
        assert!(colors.iter().all(|p| p[..3] == [GRAY[2], GRAY[1], GRAY[0]]));
    }
}
