//! Real-world model fixtures: the bundled Wine Fox bbmodel and a community
//! .ysm package must convert, and previews must render non-trivially.

use horizon_assets::player_model::{convert, convert_ysm, encode_png, peek_model_name, render_preview};
use serde_json::Value;

fn count_polygons(node: &Value) -> usize {
    let own = node["polygons"].as_array().map(Vec::len).unwrap_or(0);
    own
        + node["children"]
            .as_object()
            .map(|m| m.values().map(count_polygons).sum())
            .unwrap_or(0)
}

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/{}",
        env!("CARGO_MANIFEST_DIR"),
        name
    );
    std::fs::read(path).unwrap()
}

#[test]
fn winefox_bbmodel_converts() {
    let bytes = fixture("winefox.bbmodel");
    assert_eq!(
        peek_model_name(&bytes).as_deref(),
        Some("大正女仆酒狐"),
        "bbmodel name peeks"
    );
    let pack = convert(&bytes, &Value::Null).expect("winefox converts");
    let children = pack.model["children"].as_object().expect("root children");
    assert!(!children.is_empty(), "winefox has bones");
    let polys = count_polygons(&pack.model);
    assert!(polys > 500, "a 490-element model bakes many faces, got {polys}");
    let (w, h, rgba) = pack.texture;
    assert!(w > 0 && h > 0, "atlas sized {w}x{h}");
    assert_eq!(rgba.len(), (w * h * 4) as usize);
    // Some pixels must be opaque (the texture is really sampled).
    assert!(rgba.chunks_exact(4).any(|p| p[3] > 0), "atlas has content");
}

#[test]
fn winefox_preview_renders() {
    let pack = convert(&fixture("winefox.bbmodel"), &Value::Null).unwrap();
    let (w, h, rgba) = render_preview(&pack, 256);
    assert_eq!((w, h), (256, 256));
    let painted = rgba.chunks_exact(4).filter(|p| p[3] > 0).count();
    assert!(
        painted > 2000,
        "the model should fill a real part of the frame, got {painted}"
    );
    let png = encode_png(w, h, &rgba).expect("png encodes");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    std::fs::write("/tmp/winefox-preview.png", &png).ok();
}

#[test]
fn sample_ysm_converts() {
    let bytes = fixture("ds_神秘小骑士.ysm");
    assert!(
        peek_model_name(&bytes).is_some(),
        "ysm metadata peeks"
    );
    let pack = convert_ysm(&bytes, None).expect("ysm converts");
    let children = pack.model["children"].as_object().expect("root children");
    assert!(!children.is_empty(), "ysm bones transcode");
    let total = count_polygons(&pack.model);
    assert!(total > 100, "a real model has many faces, got {total}");
    let (w, h, rgba) = pack.texture;
    assert!(w > 0 && h > 0 && rgba.len() == (w * h * 4) as usize);
    // Texture switching: index 0 must exist and produce a pack too.
    let _alt = convert_ysm(&bytes, Some(0)).expect("texture 0 exists");
    assert!(convert_ysm(&bytes, Some(99)).is_err(), "out-of-range texture");
}

#[test]
fn sample_ysm_preview_renders() {
    let pack = convert_ysm(&fixture("ds_神秘小骑士.ysm"), None).unwrap();
    let (w, h, rgba) = render_preview(&pack, 256);
    let painted = rgba.chunks_exact(4).filter(|p| p[3] > 0).count();
    assert!(painted > 2000, "preview painted {painted}");
    std::fs::write("/tmp/ysm-preview.png", &encode_png(w, h, &rgba).unwrap()).ok();
}
