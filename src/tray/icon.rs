//! トレイアイコンを実行時に描く。アイコンのファイルを持たずに済ませるため。
//! 中間の明るさの色にして、ダーク・ライトどちらのタスクバーでも見えるようにする。

use anyhow::Result;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
};

use super::view::Status;

#[derive(Clone, Copy)]
enum Shape {
    Disc,
    Ring,
}

/// 接続中は緑の円、切断中は灰色の円、状態が分からないときは灰色の輪
fn look(status: Status) -> (Shape, [u8; 3]) {
    match status {
        Status::Connected => (Shape::Disc, [0x22, 0xC5, 0x5E]),
        Status::Disconnected => (Shape::Disc, [0x94, 0xA3, 0xB8]),
        Status::NoDongle | Status::Checking => (Shape::Ring, [0x94, 0xA3, 0xB8]),
    }
}

/// 上から下への BGRA。縁は 4x4 のサンプリングでなめらかにする
fn pixels(size: usize, shape: Shape, [r, g, b]: [u8; 3]) -> Vec<u8> {
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

pub struct Icons {
    connected: HICON,
    disconnected: HICON,
    unknown: HICON,
}

impl Icons {
    pub fn new() -> Result<Self> {
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as usize;
        Ok(Self {
            connected: draw(size, Status::Connected)?,
            disconnected: draw(size, Status::Disconnected)?,
            unknown: draw(size, Status::NoDongle)?,
        })
    }

    pub fn get(&self, status: Status) -> HICON {
        match status {
            Status::Connected => self.connected,
            Status::Disconnected => self.disconnected,
            Status::NoDongle | Status::Checking => self.unknown,
        }
    }
}

impl Drop for Icons {
    fn drop(&mut self) {
        for icon in [self.connected, self.disconnected, self.unknown] {
            let _ = unsafe { DestroyIcon(icon) };
        }
    }
}

fn draw(size: usize, status: Status) -> Result<HICON> {
    let (shape, rgb) = look(status);
    let px = pixels(size, shape, rgb);
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            biHeight: -(size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u8, px.len());
        // 32 ビットのカラーにアルファがあれば、マスクは全面 0（不透明扱い）でよい
        let mask_bits = vec![0u8; size.div_ceil(16) * 2 * size];
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, Some(mask_bits.as_ptr().cast()));
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        });
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        Ok(icon?)
    }
}
