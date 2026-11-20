// Regression: the winefox fixture must bake as a nested skeleton (not flat),
// keeping every bone's world position near the body (no "super disassembly").
use horizon_assets::player_model::convert;

fn depth_of(node: &serde_json::Value, depth: usize, out: &mut usize) {
    *out = (*out).max(depth);
    if let Some(kids) = node["children"].as_object() {
        for kid in kids.values() {
            depth_of(kid, depth + 1, out);
        }
    }
}

fn count_parts(node: &serde_json::Value) -> usize {
    1 + node["children"]
        .as_object()
        .map(|kids| kids.values().map(count_parts).sum::<usize>())
        .unwrap_or(0)
}

fn collect_world(node: &serde_json::Value, parent: [f64; 3], out: &mut Vec<[f64; 3]>) {
    let pose = |i: usize| node["pose"][i].as_f64().unwrap_or(0.0);
    let p = [parent[0] + pose(0), parent[1] + pose(1), parent[2] + pose(2)];
    for poly in node["polygons"].as_array().into_iter().flatten() {
        for v in poly.as_array().into_iter().flatten() {
            let a: Vec<f64> = v
                .as_array()
                .map(|c| c.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect())
                .unwrap_or_default();
            if a.len() >= 3 {
                out.push([p[0] + a[0], p[1] + a[1], p[2] + a[2]]);
            }
        }
    }
    if let Some(kids) = node["children"].as_object() {
        for kid in kids.values() {
            collect_world(kid, p, out);
        }
    }
}

#[test]
fn winefox_bakes_nested_without_scatter() {
    let bytes = std::fs::read("tests/fixtures/winefox.bbmodel").unwrap();
    let pack = convert(&bytes, &serde_json::Value::Null).unwrap();
    let kids = pack.model["children"].as_object().unwrap();
    // Top level only: GuiRoot, the three molang bones (Backgrounds is junk).
    assert_eq!(kids.len(), 4, "root children: {}", kids.len());
    assert!(kids.get("head").is_none(), "the head bone is nested, not a root");
    let mut max_depth = 0;
    depth_of(&pack.model, 0, &mut max_depth);
    assert!(max_depth >= 6, "nested tree preserved, depth {max_depth}");
    // The YSM companion fox (`FOX`, 32 bones) is molang-gated and never
    // bakes, so the human rig alone is left.
    assert!(kids.get("FOX").is_none(), "the roaming fox is filtered");
    let parts = count_parts(&pack.model);
    assert!(parts >= 140, "all human bones kept: {parts}");
    // World positions (pose translations accumulate; pose rotations are 0
    // because rest rotations bake into the polygons) stay near the body.
    let mut verts = Vec::new();
    collect_world(&pack.model, [0.0; 3], &mut verts);
    assert!(!verts.is_empty());
    let (mut x0, mut x1) = (f64::MAX, f64::MIN);
    let (mut y0, mut y1) = (f64::MAX, f64::MIN);
    let (mut z0, mut z1) = (f64::MAX, f64::MIN);
    for [x, y, z] in &verts {
        x0 = x0.min(*x); x1 = x1.max(*x);
        y0 = y0.min(*y); y1 = y1.max(*y);
        z0 = z0.min(*z); z1 = z1.max(*z);
    }
    // The old flat bake scattered ±31 units; a correct bake stays near the
    // 24-unit-tall body (props may reach a little further).
    assert!(x1 - x0 < 45.0, "x spread [{x0:.1}, {x1:.1}]");
    assert!(y1 - y0 < 60.0, "y spread [{y0:.1}, {y1:.1}]");
    assert!(z1 - z0 < 45.0, "z spread [{z0:.1}, {z1:.1}]");
}

#[test]
fn walk_animation_drives_legs() {
    let bytes = std::fs::read("tests/fixtures/winefox.bbmodel").unwrap();
    let pack = convert(&bytes, &serde_json::Value::Null).unwrap();
    let walk = &pack.animations["walk"];
    let legs: Vec<&serde_json::Value> = walk["bones"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(n, _)| n.starts_with("left_leg") || n.starts_with("right_leg"))
        .map(|(_, v)| v)
        .collect();
    assert!(!legs.is_empty(), "walk drives the mapped leg parts");
    let rot = |bone: &serde_json::Value, t: f64| -> f64 {
        for ch in bone.as_array().unwrap() {
            if ch["target"] == "rotation" {
                for kf in ch["keyframes"].as_array().unwrap() {
                    if kf[0].as_f64().unwrap() >= t {
                        return kf[2][0].as_f64().unwrap();
                    }
                }
            }
        }
        0.0
    };
    let left_rot = rot(legs[0], 0.0);
    let right_rot = legs
        .iter()
        .skip(1)
        .map(|v| rot(v, 0.0))
        .find(|v| (*v - left_rot).abs() > 1e-6)
        .unwrap_or(0.0);
    println!("legs {}: t0 left={left_rot:.3} other={right_rot:.3}", legs.len());
    assert!((left_rot - right_rot).abs() > 0.05, "the legs swing oppositely");
}

/// Every part name must be unique across the whole tree: the engine's pose
/// map is flat, and a hand mapped onto `left_arm` (or a foot onto
/// `left_leg`) made the real arm's pose win — the hand drifted out beside
/// the waist and the foot slid up into the calf.
#[test]
fn part_names_are_unique_across_the_tree() {
    use std::collections::HashMap;
    let bytes = std::fs::read("tests/fixtures/winefox.bbmodel").unwrap();
    let pack = horizon_assets::player_model::convert(&bytes, &serde_json::Value::Null).unwrap();
    let mut seen: HashMap<String, usize> = HashMap::new();
    fn walk(node: &serde_json::Value, name: &str, seen: &mut HashMap<String, usize>) {
        *seen.entry(name.to_owned()).or_insert(0) += 1;
        for (child, value) in node["children"].as_object().into_iter().flatten() {
            walk(value, child, seen);
        }
    }
    for (name, value) in pack.model["children"].as_object().unwrap() {
        walk(value, name, &mut seen);
    }
    let duplicates: Vec<_> = seen.iter().filter(|(_, n)| **n > 1).collect();
    assert!(duplicates.is_empty(), "duplicate part names: {duplicates:?}");
    for wanted in ["left_foot", "right_foot", "left_hand", "right_hand"] {
        assert!(seen.contains_key(wanted), "{wanted} keeps its own name");
    }
    // The foot hangs at the bottom of the leg chain, not tucked into the calf.
    fn pivot(node: &serde_json::Value, name: &str, acc: [f64; 3]) -> Option<[f64; 3]> {
        let pose = node["pose"].as_array()?;
        let here = [
            acc[0] + pose[0].as_f64()?,
            acc[1] + pose[1].as_f64()?,
            acc[2] + pose[2].as_f64()?,
        ];
        if name == "left_foot" {
            return Some(here);
        }
        node["children"]
            .as_object()?
            .iter()
            .find_map(|(child, value)| pivot(value, child, here))
    }
    let foot = pack.model["children"]
        .as_object()
        .unwrap()
        .iter()
        .find_map(|(name, value)| pivot(value, name, [0.0; 3]))
        .expect("left_foot is in the tree");
    println!("left_foot pivot {foot:?}");
    assert!(
        foot[1] > 18.0 && foot[1] < 24.0,
        "the foot sits at the ankle, got {foot:?}"
    );
}

/// A YSM pack keeps every prop it can ever hold as its own top-level bone
/// (a bow, a shield, a health bottle, the mod's logo), parked far outside
/// the body for a molang to reveal. Without molang the bake drew all of
/// them beside the player — the "other models all appear" complaint.
#[test]
fn stray_prop_roots_are_dropped() {
    let bytes = std::fs::read("tests/fixtures/ds_神秘小骑士.ysm").unwrap();
    let pack = horizon_assets::player_model::convert_ysm(&bytes, Some(0)).unwrap();
    fn walk(
        node: &serde_json::Value,
        parent: [f64; 3],
        lo: &mut f64,
        hi: &mut f64,
        foot: &mut Option<[f64; 3]>,
    ) {
        let f = |i: usize| node["pose"][i].as_f64().unwrap_or(0.0);
        let pivot = [
            parent[0] + f(0),
            parent[1] + f(1),
            parent[2] + f(2),
        ];
        for poly in node["polygons"].as_array().into_iter().flatten() {
            for v in poly.as_array().into_iter().flatten() {
                let x = v[0].as_f64().unwrap_or(0.0) + pivot[0];
                *lo = lo.min(x);
                *hi = hi.max(x);
            }
        }
        for (k, v) in node["children"].as_object().into_iter().flatten() {
            if k == "left_foot" {
                *foot = Some(pivot);
            }
            walk(v, pivot, lo, hi, foot);
        }
    }
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    let mut foot = None;
    for v in pack.model["children"].as_object().unwrap().values() {
        walk(v, [0.0; 3], &mut lo, &mut hi, &mut foot);
    }
    println!("x extent [{lo:.1}, {hi:.1}] foot={foot:?}");
    assert!(
        lo > -22.0 && hi < 22.0,
        "props parked at x ±30 must not bake into the body, got [{lo}, {hi}]"
    );
    assert!(foot.is_some(), "the feet survive the prop filter");
}

/// The 1×1×1 marker cube YSM puts on each hand pivot must not draw: it is a
/// single dark pixel beside the hand in game.
#[test]
fn hand_markers_do_not_draw() {
    let bytes = std::fs::read("tests/fixtures/winefox.bbmodel").unwrap();
    let pack = horizon_assets::player_model::convert(&bytes, &serde_json::Value::Null).unwrap();
    fn find<'a>(node: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
        if node["children"].get(name).is_some() {
            return node["children"].get(name);
        }
        node["children"]
            .as_object()?
            .values()
            .find_map(|c| find(c, name))
    }
    for hand in ["left_hand", "right_hand"] {
        let part = find(&pack.model, hand).unwrap_or_else(|| panic!("{hand} exists"));
        let quads = part["polygons"].as_array().map_or(0, Vec::len);
        println!("{hand} quads={quads}");
        assert_eq!(quads, 0, "{hand} should only carry attachment markers");
    }
}

/// The rest rotation lives in the pose, not baked into the vertices: the
/// downstream matrix applies it once, so a posed parent carries its children.
/// Baking it (the old way) left every child at a different scale of the
/// model — the "head and body split" screenshots.
#[test]
fn rest_rotation_is_carried_not_baked() {
    let bytes = std::fs::read("tests/fixtures/ds_神秘小骑士.ysm").unwrap();
    let pack = horizon_assets::player_model::convert_ysm(&bytes, Some(0)).unwrap();
    let mut with_rotation = 0usize;
    fn walk(node: &serde_json::Value, count: &mut usize) {
        let r = [
            node["pose"][3].as_f64().unwrap_or(0.0),
            node["pose"][4].as_f64().unwrap_or(0.0),
            node["pose"][5].as_f64().unwrap_or(0.0),
        ];
        if r.iter().any(|v| v.abs() > 1e-4) {
            *count += 1;
        }
        for child in node["children"].as_object().into_iter().flat_map(|o| o.values()) {
            walk(child, count);
        }
    }
    for child in pack.model["children"].as_object().unwrap().values() {
        walk(child, &mut with_rotation);
    }
    println!("parts with a rest rotation: {with_rotation}");
    assert!(with_rotation > 8, "the pack poses its bones; keep the angles");
}

/// The running basis down the bone tree (`parent * T(pose) * R(pose.rgb)`),
/// which is what the game's `render_model` composes. `collect_world` above
/// only accumulates the translations; a posed ancestor needs the rotation.
fn basis(e: [f64; 3]) -> [[f64; 3]; 3] {
    let (sx, cx) = e[0].sin_cos();
    let (sy, cy) = e[1].sin_cos();
    let (sz, cz) = e[2].sin_cos();
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
    let mul = |a: [[f64; 3]; 3], b: [[f64; 3]; 3]| {
        let mut o = [[0.0; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                o[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
            }
        }
        o
    };
    mul(mul(rz, ry), rx)
}

fn apply(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

fn walk_boxes(
    node: &serde_json::Value,
    t: [f64; 3],
    r: [[f64; 3]; 3],
    path: &str,
    out: &mut Vec<(String, [f64; 3], [f64; 3])>,
) {
    let pose = |i: usize| node["pose"][i].as_f64().unwrap_or(if i >= 6 { 1.0 } else { 0.0 });
    let d = apply(r, [pose(0), pose(1), pose(2)]);
    let pivot = [t[0] + d[0], t[1] + d[1], t[2] + d[2]];
    let rot = {
        let mut m = [[0.0; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] = (0..3).map(|k| r[i][k] * basis([pose(3), pose(4), pose(5)])[k][j]).sum();
            }
        }
        m
    };
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for poly in node["polygons"].as_array().into_iter().flatten() {
        for v in poly.as_array().into_iter().flatten() {
            let a: Vec<f64> = v
                .as_array()
                .map(|c| c.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect())
                .unwrap_or_default();
            if a.len() < 3 {
                continue;
            }
            let lp = apply(rot, [a[0], a[1], a[2]]);
            for i in 0..3 {
                lo[i] = lo[i].min(pivot[i] + lp[i]);
                hi[i] = hi[i].max(pivot[i] + lp[i]);
            }
        }
    }
    if lo[0] <= hi[0] {
        out.push((path.to_owned(), lo, hi));
    }
    for (name, child) in node["children"].as_object().into_iter().flatten() {
        walk_boxes(child, pivot, rot, &format!("{path}/{name}"), out);
    }
}

/// A bone leaning about x (Tail 32.5°, Tail2 -127.5°) keeps its lean only if
/// the mapping's 180° z turn is applied as a conjugation: x and y reverse,
/// z stays. Treating it as a plain x-plane reflection (x kept, z negated)
/// folded the fox's tail down *through* the legs — the "tail stuck in the
/// front" screenshots — so pin the tip behind the body and above the neck.
#[test]
fn rest_rotation_keeps_the_tail_behind_and_up() {
    let bytes = std::fs::read("tests/fixtures/winefox.bbmodel").unwrap();
    let pack = convert(&bytes, &serde_json::Value::Null).unwrap();
    let mut boxes = Vec::new();
    walk_boxes(
        &pack.model,
        [0.0; 3],
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        "",
        &mut boxes,
    );
    let tail = boxes
        .iter()
        .find(|(path, _, _)| path.contains("/tail7"))
        .expect("the tail's tip bone bakes");
    let centre_z = (tail.1[2] + tail.2[2]) / 2.0;
    let centre_y = (tail.1[1] + tail.2[1]) / 2.0;
    println!("tail tip z={centre_z:.1} y={centre_y:.1}");
    assert!(centre_z > 12.0, "tail behind the body: z={centre_z:.1}");
    assert!(centre_y < 0.0, "tail raised above the neck: y={centre_y:.1}");
}

/// A `.ysm` pack carries its animations in `animations/*.json` (bedrock
/// format) and its roulette menu in `ysm.json`. Both used to be dropped, so
/// the pack stood in its rest pose and the wheel offered nothing.
#[test]
fn ysm_pack_bakes_its_animations_and_wheel_actions() {
    use horizon_assets::player_model::{convert_ysm, ysm_actions};
    let bytes = std::fs::read("tests/fixtures/ds_神秘小骑士.ysm").unwrap();
    let pack = convert_ysm(&bytes, Some(0)).unwrap();
    let anims = pack.animations.as_object().expect("animations baked");
    for name in ["idle", "walk", "run", "ani1", "extra1"] {
        assert!(anims.contains_key(name), "missing animation {name}");
    }
    let walk = &anims["walk"]["bones"];
    assert!(
        walk.get("left_arm").is_some() && walk.get("right_arm").is_some(),
        "walk drives the arms by part name: {}",
        walk.to_string().chars().take(160).collect::<String>()
    );
    let actions = ysm_actions(&bytes);
    assert!(
        actions.iter().any(|(key, label)| key == "ani1" && label == "朝下指"),
        "declared wheel actions: {actions:?}"
    );
    assert!(actions.len() >= 8, "both menus merge: {}", actions.len());
}

/// Blockbench groups and elements carry a `visibility` flag, and YSM packs
/// use it for spare shapes: the fox form, alt outfits, the props a molang
/// shows. Baking them drew extra limbs over the player (the "a second pair
/// of hands floats in front of the body" screenshots), so a switched-off
/// subtree must not reach the part tree.
#[test]
fn switched_off_shapes_do_not_bake() {
    let bytes = std::fs::read("tests/fixtures/winefox.bbmodel").unwrap();
    let raw: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    // The fixture must actually carry a hidden group, or this proves nothing.
    assert!(
        raw.to_string().contains("\"visibility\":false"),
        "fixture lost its hidden shapes"
    );
    let pack = convert(&bytes, &serde_json::Value::Null).unwrap();
    fn names(node: &serde_json::Value, out: &mut Vec<String>) {
        for (name, child) in node["children"].as_object().into_iter().flatten() {
            out.push(name.to_ascii_lowercase());
            names(child, out);
        }
    }
    let mut all = Vec::new();
    names(&pack.model, &mut all);
    // The fox form is the hidden `FOX` subtree (AllBody2 / Head2 / FoxEye...).
    for needle in ["fox", "head2", "arm2", "hand2", "upbody2"] {
        assert!(
            !all.iter().any(|n| n.contains(needle)),
            "hidden shape `{needle}` still bakes: {all:?}"
        );
    }
}

/// A pack's own hand clips drive a used item and a swing; the wheel and the
/// vanilla fallback both key off these names.
#[test]
fn hand_clips_bake() {
    let bytes = std::fs::read("tests/fixtures/winefox.bbmodel").unwrap();
    let pack = convert(&bytes, &serde_json::Value::Null).unwrap();
    let anims = pack.animations.as_object().unwrap();
    for name in ["use_mainhand", "swing_hand"] {
        let anim = anims.get(name).unwrap_or_else(|| panic!("missing {name}"));
        let bones = anim["bones"].as_object().unwrap();
        assert!(
            bones.contains_key("left_arm") || bones.contains_key("right_arm"),
            "{name} drives no arm: {:?}",
            bones.keys().collect::<Vec<_>>()
        );
    }
}
