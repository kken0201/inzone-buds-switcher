//! exe にアイコンを埋め込む。アイコンはトレイの「接続中」と同じ絵をここで描き、
//! 画像ファイルをリポジトリに持たずに済ませる。

#[allow(dead_code)]
#[path = "src/tray/shape.rs"]
mod shape;

use std::path::PathBuf;

/// エクスプローラーが表示に使う大きさ。256 は大アイコン表示用
const SIZES: [usize; 6] = [16, 24, 32, 48, 64, 256];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/tray/shape.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let ico = out.join("app.ico");
    std::fs::write(&ico, ico_file(&SIZES)).unwrap();
    let rc = out.join("app.rc");
    let ico_path = ico.to_str().unwrap().replace('\\', "\\\\");
    std::fs::write(&rc, format!("1 ICON \"{ico_path}\"\n")).unwrap();
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().unwrap();
}

/// 32 ビット BMP 形式の画像を並べた .ico
fn ico_file(sizes: &[usize]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| bmp_image(s)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (&size, image) in sizes.iter().zip(&images) {
        // 256 は 0 で表す
        let dim = if size >= 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += image.len();
    }
    for image in images {
        out.extend_from_slice(&image);
    }
    out
}

/// BITMAPINFOHEADER + 下から上への BGRA + AND マスク。高さはマスクの分を含めて 2 倍で書く
fn bmp_image(size: usize) -> Vec<u8> {
    let px = shape::pixels(size, shape::Shape::Disc, shape::GREEN);
    let mut out = Vec::new();
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(size as i32).to_le_bytes());
    out.extend_from_slice(&(size as i32 * 2).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for row in px.chunks(size * 4).rev() {
        out.extend_from_slice(row);
    }
    // アルファがあればマスクは使われないので全面 0
    let mask_row = size.div_ceil(32) * 4;
    out.resize(out.len() + mask_row * size, 0);
    out
}
