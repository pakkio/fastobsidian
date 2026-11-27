//! Embeds the app icon in the Windows .exe, so Explorer, the taskbar and shortcuts show it.
//!
//! The icon is drawn by src/icon.rs (the same code as the window icon) and written as a compiled
//! resource file (.res), which the MSVC linker takes directly: no resource compiler or extra crate.

include!("src/icon.rs");

use std::path::PathBuf;

/// Icon sizes in the .exe: the small ones for lists and the taskbar, 256 for Explorer's large views.
const SIZES: [u32; 7] = [16, 20, 24, 32, 48, 64, 256];
/// Resource id of the icon group; the first group in an .exe is its file icon.
const GROUP_ID: u16 = 1;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon.rs");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os != "windows" {
        return;
    }
    if env != "msvc" {
        println!("cargo:warning=the .exe icon is embedded only with the MSVC toolchain");
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("fastobsidian-icon.res");
    std::fs::write(&out, icon_res()).expect("write icon resource");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

/// One icon image as a 32-bit DIB, the format icon resources use: header, BGRA rows from the bottom, then the
/// 1-bit transparency mask (also from the bottom, rows padded to 32 bits).
fn dib(n: u32) -> Vec<u8> {
    let rgba = crystal_rgba(n);
    let mut d = Vec::new();
    let le32 = |d: &mut Vec<u8>, v: u32| d.extend_from_slice(&v.to_le_bytes());
    le32(&mut d, 40); // BITMAPINFOHEADER size
    le32(&mut d, n); // width
    le32(&mut d, n * 2); // height: image + mask
    d.extend_from_slice(&1u16.to_le_bytes()); // planes
    d.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    for _ in 0..6 {
        le32(&mut d, 0); // no compression, default sizes and colors
    }
    for y in (0..n).rev() {
        for x in 0..n {
            let i = ((y * n + x) * 4) as usize;
            d.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    let row = n.div_ceil(32) * 4;
    for y in (0..n).rev() {
        let mut bits = vec![0u8; row as usize];
        for x in 0..n {
            if rgba[((y * n + x) * 4 + 3) as usize] == 0 {
                bits[(x / 8) as usize] |= 0x80 >> (x % 8);
            }
        }
        d.extend_from_slice(&bits);
    }
    d
}

/// A compiled resource file: an empty first entry, one RT_ICON per size, then the RT_GROUP_ICON listing them.
fn icon_res() -> Vec<u8> {
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    fn entry(res: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
        let le32 = |v: u32| v.to_le_bytes();
        let le16 = |v: u16| v.to_le_bytes();
        res.extend_from_slice(&le32(data.len() as u32)); // data size
        res.extend_from_slice(&le32(32)); // header size
        res.extend_from_slice(&le16(0xFFFF)); // type by number
        res.extend_from_slice(&le16(kind));
        res.extend_from_slice(&le16(0xFFFF)); // name by number
        res.extend_from_slice(&le16(id));
        res.extend_from_slice(&le32(0)); // data version
        res.extend_from_slice(&le16(flags));
        res.extend_from_slice(&le16(0x0409)); // language: en-US
        res.extend_from_slice(&le32(0)); // version
        res.extend_from_slice(&le32(0)); // characteristics
        res.extend_from_slice(data);
        while !res.len().is_multiple_of(4) {
            res.push(0);
        }
    }
    let mut res = Vec::new();
    entry(&mut res, 0, 0, 0, &[]);
    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes()); // reserved
    group.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    group.extend_from_slice(&(SIZES.len() as u16).to_le_bytes());
    for (i, &n) in SIZES.iter().enumerate() {
        let id = i as u16 + 1;
        let image = dib(n);
        entry(&mut res, RT_ICON, id, 0x1010, &image); // moveable, discardable
        let side = if n >= 256 { 0 } else { n as u8 }; // 0 means 256
        group.extend_from_slice(&[side, side, 0, 0]); // width, height, colors, reserved
        group.extend_from_slice(&1u16.to_le_bytes()); // planes
        group.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        group.extend_from_slice(&(image.len() as u32).to_le_bytes());
        group.extend_from_slice(&id.to_le_bytes());
    }
    entry(&mut res, RT_GROUP_ICON, GROUP_ID, 0x1030, &group); // moveable, pure, discardable
    res
}
