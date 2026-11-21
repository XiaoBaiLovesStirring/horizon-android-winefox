//! 内置的车万女仆（Touhou Little Maid）模型。
//!
//! 这里放的是 TLM 默认模型（博丽灵梦，`hakurei_reimu`）烘焙后的结果：
//! 1.10 的 Bedrock 几何经 `player_model::convert_ysm` 烘成引擎的
//! `Part` 结构，骨骼名改成原版人形的那套（`head` / `body` / `left_arm`
//! / `right_arm` / `left_leg` / `right_leg`），于是可以直接吃
//! `Anim::Humanoid` 的走路、挥手、潜行、睡觉动作。
//!
//! 实体本身仍是 `minecraft:mannequin`（模特）：存档、网络 id、`/summon`
//! 都不用改，`MobTypes::load` 把模特换成本模型（见 `mobs.rs`）。

use crate::mobs::Part;
use std::sync::OnceLock;

/// 图集里女仆贴图的 sprite 名。
pub const TEXTURE: &str = "horizon/maid";

const MODEL_JSON: &str = include_str!("../assets/maid/maid.json");

/// 贴图 PNG（图集构建时上传成 [`TEXTURE`]）。
pub const TEXTURE_PNG: &[u8] = include_bytes!("../assets/maid/maid.png");

static MODEL: OnceLock<Part> = OnceLock::new();

/// 女仆的模型树（烘焙好的 `Part`，只解析一次）。
pub fn model() -> Part {
    MODEL
        .get_or_init(|| {
            let v: serde_json::Value =
                serde_json::from_str(MODEL_JSON).expect("the baked maid model parses");
            Part::from_value("root", &v)
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 随包的模型得是完整的人形骨架：`humanoid()` 按名字找这几根骨头，
    /// 少一根那条肢体就不动。
    #[test]
    fn the_bundled_model_is_a_full_humanoid() {
        let model = model();
        for name in [
            "head", "body", "left_arm", "right_arm", "left_leg", "right_leg",
        ] {
            assert!(
                model.children.iter().any(|c| c.name == name),
                "the maid model has no {name} bone"
            );
        }
        fn walk(p: &Part) -> usize {
            p.polygons.len() + p.children.iter().map(walk).sum::<usize>()
        }
        let faces = walk(&model);
        println!("maid faces = {faces}");
        assert!(faces > 100, "the maid model baked {faces} faces");
    }
}
