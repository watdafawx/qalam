// Embed the app manifest (modern control styles for task dialogs, per-monitor DPI awareness)
// and the app icon (assets/qalam.ico; the taskbar and Explorer read it from the exe).
use std::path::Path;

fn main() {
    let root = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest = Path::new(&root).join("qalam.manifest");
    println!("cargo:rerun-if-changed=qalam.manifest");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());

    let ico = Path::new(&root).join("assets").join("qalam.ico");
    println!("cargo:rerun-if-changed=assets/qalam.ico");
    let res = Path::new(&std::env::var("OUT_DIR").unwrap()).join("qalam_icon.res");
    std::fs::write(&res, icon_res(&std::fs::read(ico).unwrap())).unwrap();
    println!("cargo:rustc-link-arg-bins={}", res.display());
}

/// A compiled resource (.res) file holding the .ico's images as RT_ICON 1..n and RT_GROUP_ICON 1.
fn icon_res(ico: &[u8]) -> Vec<u8> {
    let u16_at = |o: usize| u16::from_le_bytes([ico[o], ico[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes([ico[o], ico[o + 1], ico[o + 2], ico[o + 3]]);
    let count = u16_at(4) as usize;
    let mut out = Vec::new();
    // Every .res starts with an empty entry.
    entry(&mut out, 0, 0, &[]);
    let mut group = vec![0, 0, 1, 0];
    group.extend_from_slice(&(count as u16).to_le_bytes());
    for i in 0..count {
        let e = 6 + i * 16;
        let (size, offset) = (u32_at(e + 8) as usize, u32_at(e + 12) as usize);
        entry(&mut out, 3, (i + 1) as u16, &ico[offset..offset + size]);
        group.extend_from_slice(&ico[e..e + 8]); // width, height, colors, reserved, planes, bit count
        group.extend_from_slice(&(size as u32).to_le_bytes());
        group.extend_from_slice(&((i + 1) as u16).to_le_bytes());
    }
    entry(&mut out, 14, 1, &group);
    out
}

fn entry(out: &mut Vec<u8>, kind: u16, id: u16, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&32u32.to_le_bytes()); // header size
    for v in [0xFFFFu16, kind, 0xFFFF, id] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&0u32.to_le_bytes()); // data version
    out.extend_from_slice(&(if kind == 0 { 0u16 } else { 0x1030u16 }).to_le_bytes()); // moveable|pure|discardable
    out.extend_from_slice(&(if kind == 0 { 0u16 } else { 0x0409u16 }).to_le_bytes()); // language
    out.extend_from_slice(&0u32.to_le_bytes()); // version
    out.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    out.extend_from_slice(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}
