//! トレイアイコンを実行時に描く。アイコンのファイルを持たずに済ませるため。
//! 中間の明るさの色にして、ダーク・ライトどちらのタスクバーでも見えるようにする。

use std::collections::HashMap;

use anyhow::Result;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
};

use super::shape::{GRAY, Shape, batteries, pixels};
use super::view::Icon;

/// 描いたアイコンを絵ごとに持ち、同じ絵を描き直さない。
/// 絵は左右それぞれ 13 通り（空の枠、0–5 段 × 赤かどうか）の組み合わせと 2 状態に限られるので、捨てずに持ち続ける
pub struct Icons {
    size: usize,
    drawn: HashMap<Icon, HICON>,
}

impl Icons {
    pub fn new() -> Self {
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as usize;
        Self { size, drawn: HashMap::new() }
    }

    pub fn get(&mut self, icon: Icon) -> Result<HICON> {
        if let Some(&h) = self.drawn.get(&icon) {
            return Ok(h);
        }
        let h = draw(self.size, icon)?;
        self.drawn.insert(icon, h);
        Ok(h)
    }
}

impl Drop for Icons {
    fn drop(&mut self) {
        for &icon in self.drawn.values() {
            let _ = unsafe { DestroyIcon(icon) };
        }
    }
}

/// 切断中は灰色の円、状態が分からないときは灰色の輪。形を変え、色を見分けにくくても区別できるようにする
fn draw(size: usize, icon: Icon) -> Result<HICON> {
    let px = match icon {
        Icon::Batteries(gauges) => batteries(size, gauges),
        Icon::Disconnected => pixels(size, Shape::Disc, GRAY),
        Icon::Unknown => pixels(size, Shape::Ring, GRAY),
    };
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
