//! 内置女仆的模型必须真的能画出面：烘焙坏了、骨骼名对不上 `humanoid()`、
//! 或者 Poses 把它整枝隐藏，`/summon minecraft:maid` 都会变成一团空气。

use std::path::Path;
use std::sync::Arc;

use horizon_client::mobs::{render, Mob, MobTypes};

#[test]
fn the_built_in_maid_model_draws_faces() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local/horizon-client")
        .join(horizon_core::VANILLA_VERSION);
    if !root.join("entity-types.json").exists() {
        return;
    }
    let types = MobTypes::load(&root).expect("the types load");
    let kind = types
        .types
        .get("minecraft:maid")
        .expect("the maid type")
        .clone();
    let (model_key, texture) = render::body(&Mob::new(
        1,
        Arc::new(kind.clone()),
        horizon_core::geometry::Vec3::new(0.0, 0.0, 0.0),
        0.0,
    ));
    assert_eq!(texture, "horizon/maid");
    // The type has no layer of its own: the baked model is the body.
    assert!(kind.layer(&model_key).is_none());

    let m = Mob::new(
        1,
        Arc::new(kind),
        horizon_core::geometry::Vec3::new(0.0, 0.0, 0.0),
        0.0,
    );
    let mut out = Vec::new();
    render::render_single(
        &m,
        0.0,
        glam::DVec3::ZERO,
        [0.0, 0.0, 1.0, 1.0],
        0xFFFF_FFFF,
        &mut out,
    );
    assert!(!out.is_empty(), "the maid model draws faces");
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for v in &out {
        for i in 0..3 {
            lo[i] = lo[i].min(v.pos[i]);
            hi[i] = hi[i].max(v.pos[i]);
        }
    }
    // ~1.8 blocks tall and ~1.2 wide at the sleeves: not a speck, not a giant.
    let height = hi[1] - lo[1];
    assert!((1.4..2.2).contains(&height), "height {height}");
    assert!(hi[0] - lo[0] < 2.0, "width {}", hi[0] - lo[0]);
}
