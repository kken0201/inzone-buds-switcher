//! アイコンの形と色。トレイアイコン（実行時）と exe のアイコン（build.rs）の両方で使うため、
//! 標準ライブラリ以外に依存しない。

pub const GREEN: [u8; 3] = [0x22, 0xC5, 0x5E];
pub const GRAY: [u8; 3] = [0x94, 0xA3, 0xB8];

#[derive(Clone, Copy)]
pub enum Shape {
    Disc,
    Ring,
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
