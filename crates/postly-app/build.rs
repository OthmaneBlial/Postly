use std::{env, fs, path::PathBuf};

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

/// Build a minimal Vista-compatible ICO containing the canonical PNG.
/// Windows accepts PNG-compressed images inside an ICO, which preserves the
/// exact pixels used by the egui window without committing a second binary.
fn ico_with_png(png: &[u8]) -> Vec<u8> {
    let mut ico = Vec::with_capacity(22 + png.len());
    push_u16(&mut ico, 0); // reserved
    push_u16(&mut ico, 1); // icon resource
    push_u16(&mut ico, 1); // one image
    ico.extend_from_slice(&[0, 0, 0, 0]); // 512x512 (0 means 256+), RGBA
    push_u16(&mut ico, 1); // color planes
    push_u16(&mut ico, 32); // bits per pixel
    push_u32(&mut ico, png.len() as u32);
    push_u32(&mut ico, 22); // header (6) + directory entry (16)
    ico.extend_from_slice(png);
    ico
}

fn main() {
    println!("cargo:rerun-if-changed=assets/postly-icon.png");
    if env::var("CARGO_CFG_TARGET_OS").ok().as_deref() != Some("windows") {
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"));
    let icon_path = out_dir.join("postly.ico");
    let png = include_bytes!("assets/postly-icon.png");
    fs::write(&icon_path, ico_with_png(png)).expect("write generated Windows icon");

    // RC accepts forward-slash paths on both MinGW and MSVC. Escape quotes so
    // unusual build directories cannot terminate the resource declaration.
    let rc_path = out_dir.join("postly-icon.rc");
    let icon_path = icon_path
        .to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"");
    fs::write(&rc_path, format!("1 ICON \"{icon_path}\"\n"))
        .expect("write Windows resource declaration");
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_optional()
        .expect("embed Postly icon resource");
}
