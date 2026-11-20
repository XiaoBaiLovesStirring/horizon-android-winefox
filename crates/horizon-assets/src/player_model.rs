//! Custom player models (the "Yes Steve Model" workflow): a Blockbench
//! `.bbmodel` imported outside the game becomes the player's model.
//!
//! A bbmodel's `outliner` is a bone tree, each bone a Blockbench group whose
//! `origin` is the pivot; its `elements` are cubes with per-face or box UVs and
//! base64-embedded textures. The output is the game's own baked entity model
//! schema (`entity-models.json`): per part a pose `[x, y, z, xRot, yRot, zRot,
//! scale...]`, textured polygons `[x, y, z, u, v]` and normals, children by
//! name. Bones whose names match the vanilla player skeleton (`head`, `body`,
//! `right_arm`, `left_arm`, `right_leg`, `left_leg`, `hat`) are driven by the
//! game's own humanoid animations; any other bone keeps its static pose.
//!
//! Coordinate mapping (bedrock-style bbmodel, 16 units per block, ground at
//! y = 0): the model's own space mirrors the vanilla model space on X and has
//! Y up, while the game's model space has Y down with y = 0 at 1.5 blocks
//! above the ground. So `mc_x = -bb_x`, `mc_y = MC_HEIGHT - bb_y`, `mc_z =
//! bb_z`, with `MC_HEIGHT` 24 for a player-sized model (an optional
//! `<name>.json` manifest beside the bbmodel overrides it and the flips).
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// The converted model: the baked part tree as the game's own JSON schema and
/// the stitched atlas texture as RGBA rows.
pub struct ConvertedPack {
    /// The value of the `"minecraft:player#main"` entry: the root part.
    pub model: Value,
    /// The stitched texture, row-major RGBA.
    pub texture: (u32, u32, Vec<u8>),
    /// The model's keyframe animations in the game's `Animation::parse`
    /// schema (`{length, looping, bones: {part: [{target, keyframes}]}}`),
    /// rotations in radians, model-unit positions. Null when the source
    /// carries none.
    pub animations: Value,
}

/// An optional `<name>.json` beside the bbmodel calibrates the mapping.
#[derive(Clone, Copy, Debug)]
pub struct Mapping {
    /// Flip X (bedrock models mirror the vanilla player's). Default true.
    pub flip_x: bool,
    /// The bbmodel y that the game's model y = 0 sits at (the player's neck:
    /// 24). Default 24.
    pub mc_height: f64,
}

impl Default for Mapping {
    fn default() -> Self {
        Self {
            flip_x: true,
            mc_height: 24.0,
        }
    }
}

/// Converts a bbmodel file's bytes into the game's model schema. `manifest`
/// is the parsed optional `<name>.json` beside it (or null).
pub fn convert(bytes: &[u8], manifest: &Value) -> Result<ConvertedPack, String> {
    let mapping = Mapping {
        flip_x: manifest
            .get("flip_x")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        mc_height: manifest
            .get("mc_height")
            .and_then(Value::as_f64)
            .unwrap_or(24.0),
    };
    let bb = parse_bbmodel(bytes)?;
    bake(&bb, mapping, None)
}

/// A small base64 decoder (standard alphabet, padding optional): the bbmodel
/// embeds its textures as `data:image/png;base64,...` URLs.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in text.as_bytes() {
        let Some(v) = value(c) else { continue };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

/// A PNG's width, height and RGBA rows (the same rules the hud's decoder
/// uses). The bbmodel embeds its textures as PNGs.
pub fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let err = |e: String| format!("bbmodel texture: {e}");
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| err(e.to_string()))?;
    let mut raw = vec![0; reader.output_buffer_size().ok_or("texture too large")?];
    let info = reader
        .next_frame(&mut raw)
        .map_err(|e| err(e.to_string()))?;
    let channels = usize::from(info.color_type.samples());
    let mut rgba = Vec::with_capacity(raw.len());
    for p in raw[..info.buffer_size()].chunks_exact(channels) {
        match channels {
            4 => rgba.extend_from_slice(p),
            3 => rgba.extend([p[0], p[1], p[2], 255]),
            2 => rgba.extend([p[0], p[0], p[0], p[1]]),
            _ => rgba.extend([p[0], p[0], p[0], 255]),
        }
    }
    Ok((info.width, info.height, rgba))
}

#[derive(Clone)]
struct BbCube {
    from: [f64; 3],
    to: [f64; 3],
    /// Per-face uv rects in uv-canvas pixels (north, east, south, west, up,
    /// down), None where the face falls back to the box layout.
    faces: [Option<[f64; 4]>; 6],
    #[allow(dead_code)]
    box_uv: bool,
    uv_offset: [f64; 2],
    /// Which embedded texture the UVs read; none is texture 0.
    texture: Option<usize>,
    /// An optional rotation baked into the polygons: Euler degrees (bbmodel's
    /// own convention, x and z as displayed) around a pivot in model units.
    rotation: Option<([f64; 3], [f64; 3])>,
    /// Blockbench's per-element `visibility` flag.
    visible: bool,
}

struct BbBone {
    name: String,
    pivot: [f64; 3],
    rotation: [f64; 3],
    /// Blockbench's `visibility` flag. A model ships extra shapes (a fox
    /// form, alt outfits, the props a molang shows) switched off here; the
    /// geometry is real but must not draw until an animation turns it on.
    visible: bool,
    children: Vec<Child>,
}

enum Child {
    Bone(usize),
    Cube(String),
}

struct BbModel {
    cubes: HashMap<String, BbCube>,
    bones: Vec<BbBone>,
    /// The indexes of bones the outliner named at its top level.
    roots: Vec<usize>,
    /// Bone uuid -> bone index (animations address bones by uuid).
    by_uuid: HashMap<String, usize>,
    /// The bbmodel's raw `animations` array (Blockbench keyframes).
    animations: Value,
    textures: Vec<(u32, u32, Vec<u8>)>,
    uv_width: f64,
    uv_height: f64,
}

/// Blockbench's `visibility` (a bool, or a molang string we can't evaluate):
/// only an explicit false hides. YSM packs keep spare shapes — a fox form,
/// alt outfits — switched off, and drawing them puts extra limbs on the
/// player (the "two hands floating out in front" screenshots).
fn visible(v: &Value) -> bool {
    visible_flag(v.get("visibility").or_else(|| v.get("visible")))
}

fn visible_flag(flag: Option<&Value>) -> bool {
    match flag {
        Some(Value::Bool(on)) => *on,
        Some(Value::String(s)) => s != "false",
        _ => true,
    }
}

fn parse_bbmodel(bytes: &[u8]) -> Result<BbModel, String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("bbmodel JSON: {e}"))?;
    let err = |what: &str| format!("bbmodel {what}");

    // Textures: base64 data URLs, decoded.
    let mut textures = Vec::new();
    for t in v["textures"].as_array().into_iter().flatten() {
        let source = t["source"].as_str().unwrap_or_default();
        let data = source
            .split_once("base64,")
            .map(|(_, b)| b)
            .and_then(base64_decode)
            .ok_or_else(|| err("texture without base64 data"))?;
        textures.push(decode_png(&data)?);
    }
    if textures.is_empty() {
        return Err(err("has no textures"));
    }
    let (uv_width, uv_height) = (
        v["resolution"]["width"].as_f64().unwrap_or(16.0),
        v["resolution"]["height"].as_f64().unwrap_or(16.0),
    );

    // Cubes: Blockbench elements of type "cube" (meshes are skipped).
    let mut cubes = HashMap::new();
    for e in v["elements"].as_array().into_iter().flatten() {
        let uuid = e["uuid"].as_str().unwrap_or_default();
        if uuid.is_empty() || e["type"].as_str().is_some_and(|t| t != "cube") {
            continue;
        }
        let point = |k: &str| -> [f64; 3] {
            [
                e[k][0].as_f64().unwrap_or(0.0),
                e[k][1].as_f64().unwrap_or(0.0),
                e[k][2].as_f64().unwrap_or(0.0),
            ]
        };
        let rect = |name: &str| -> Option<[f64; 4]> {
            let uv = e["faces"][name]["uv"].as_array()?;
            (uv.len() == 4 && uv.iter().all(Value::is_number)).then(|| {
                [
                    uv[0].as_f64().unwrap_or(0.0),
                    uv[1].as_f64().unwrap_or(0.0),
                    uv[2].as_f64().unwrap_or(0.0),
                    uv[3].as_f64().unwrap_or(0.0),
                ]
            })
        };
        cubes.insert(
            uuid.to_owned(),
            BbCube {
                from: point("from"),
                to: point("to"),
                faces: [rect("north"), rect("east"), rect("south"), rect("west"), rect("up"), rect("down")],
                box_uv: e["box_uv"].as_bool().unwrap_or(false),
                uv_offset: [
                    e["uv_offset"][0].as_f64().unwrap_or(0.0),
                    e["uv_offset"][1].as_f64().unwrap_or(0.0),
                ],
                texture: e["faces"]["north"]["texture"]
                    .as_u64()
                    .map(|i| usize::try_from(i).unwrap_or(0)),
                // Bedrock-style bbmodels rotate cubes with an Euler array and
                // a pivot origin; java-style ones use {angle, axis, origin}.
                rotation: if e["rotation"].is_array() {
                    let pivot = point3(&e["origin"]);
                    (
                        [
                            e["rotation"][0].as_f64().unwrap_or(0.0),
                            e["rotation"][1].as_f64().unwrap_or(0.0),
                            e["rotation"][2].as_f64().unwrap_or(0.0),
                        ],
                        pivot,
                    )
                        .into()
                } else if let Some(angle) = e["rotation"]["angle"].as_f64() {
                    let axis = e["rotation"]["axis"].as_str().unwrap_or("y");
                    let mut euler = [0.0; 3];
                    match axis {
                        "x" => euler[0] = angle,
                        "z" => euler[2] = angle,
                        _ => euler[1] = angle,
                    }
                    (euler, point3(&e["rotation"]["origin"])).into()
                } else {
                    None
                },
                visible: visible(e),
            },
        );
    }

    // The bone tree: outliner groups are bones (name, origin, rotation),
    // plain strings are cube uuids; nested groups recurse. Bones are built
    // depth first, children referencing their parent's bone index.
    struct Builder {
        groups: HashMap<String, (String, [f64; 3], [f64; 3], bool, Vec<Value>)>,
        bones: Vec<BbBone>,
        by_uuid: HashMap<String, usize>,
    }
    impl Builder {
        fn build(&mut self, uuid: &str) -> Option<usize> {
            if let Some(&index) = self.by_uuid.get(uuid) {
                return Some(index);
            }
            let (name, pivot, rotation, visible, kids) = self.groups.remove(uuid)?;
            let mut children = Vec::new();
            for kid in &kids {
                let kid_uuid = kid.as_str().unwrap_or_default();
                if self.groups.contains_key(kid_uuid) {
                    if let Some(index) = self.build(kid_uuid) {
                        children.push(Child::Bone(index));
                    }
                } else {
                    children.push(Child::Cube(kid_uuid.to_owned()));
                }
            }
            let index = self.bones.len();
            self.bones.push(BbBone {
                name,
                pivot,
                rotation,
                visible,
                children,
            });
            self.by_uuid.insert(uuid.to_owned(), index);
            Some(index)
        }
    }
    let mut builder = Builder {
        groups: HashMap::new(),
        bones: Vec::new(),
        by_uuid: HashMap::new(),
    };
    fn point3(v: &Value) -> [f64; 3] {
        [
            v[0].as_f64().unwrap_or(0.0),
            v[1].as_f64().unwrap_or(0.0),
            v[2].as_f64().unwrap_or(0.0),
        ]
    }
    // All groups register first (a nested group must be in the map before
    // its parent builds, or the parent mistakes it for a cube and the tree
    // collapses flat, losing every ancestor transform), then only the
    // outliner's top-level entries become roots.
    fn visit(entry: &Value, builder: &mut Builder) {
        let Some(obj) = entry.as_object() else {
            return; // plain strings here are loose cubes, not bones
        };
        let uuid = obj
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if uuid.is_empty() {
            return;
        }
        let mut kids = Vec::new();
        for child in obj.get("children").and_then(Value::as_array).into_iter().flatten() {
            if child.is_string() {
                kids.push(child.clone());
            } else if let Some(child_uuid) = child.get("uuid").and_then(Value::as_str) {
                kids.push(Value::String(child_uuid.to_owned()));
            }
        }
        builder.groups.insert(
            uuid.clone(),
            (
                obj.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                point3(obj.get("origin").unwrap_or(&Value::Null)),
                point3(obj.get("rotation").unwrap_or(&Value::Null)),
                visible_flag(obj.get("visibility").or_else(|| obj.get("visible"))),
                kids,
            ),
        );
        for child in obj.get("children").and_then(Value::as_array).into_iter().flatten() {
            if child.is_object() {
                visit(child, builder);
            }
        }
    }
    let mut roots = Vec::new();
    for entry in v["outliner"].as_array().into_iter().flatten() {
        visit(entry, &mut builder);
    }
    for entry in v["outliner"].as_array().into_iter().flatten() {
        let Some(uuid) = entry.get("uuid").and_then(Value::as_str) else {
            continue; // plain strings here are loose cubes, not bones
        };
        if uuid.is_empty() {
            continue;
        }
        if let Some(index) = builder.build(uuid)
            && !roots.contains(&index)
        {
            roots.push(index);
        }
    }

    Ok(BbModel {
        cubes,
        bones: builder.bones,
        roots,
        by_uuid: builder.by_uuid,
        animations: v["animations"].clone(),
        textures,
        uv_width,
        uv_height,
    })
}

/// The vanilla player skeleton's part name for a bone, or None to keep the
/// bone's own lowercased name (a static pose the animations never touch).
fn part_name(bone: &str) -> Option<&'static str> {
    let key: String = bone
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    match key.as_str() {
        "head" => Some("head"),
        "body" | "torso" | "chest" => Some("body"),
        "rightarm" | "rarm" | "armr" => Some("right_arm"),
        "leftarm" | "larm" | "arml" => Some("left_arm"),
        // Hands and feet keep their own names: mapping them onto the arm /
        // leg bone (a shortcut for models whose hand *is* the arm) made two
        // parts share one name, and the pose map keeps only the first — the
        // foot inherited the leg's offset and slid into the calf, the hand
        // drifted out beside the waist. They ride their parent bone instead.
        "righthand" | "handr" => Some("right_hand"),
        "lefthand" | "handl" => Some("left_hand"),
        "rightleg" | "rleg" | "legr" => Some("right_leg"),
        "leftleg" | "lleg" | "legl" => Some("left_leg"),
        "rightfoot" | "rfoot" | "footr" => Some("right_foot"),
        "leftfoot" | "lfoot" | "footl" => Some("left_foot"),
        "hat" | "hatlayer" | "helmetlayer" => Some("hat"),
        _ => None,
    }
}

/// Box-uv face rects, vanilla's layout (the baked head's uv rects confirm
/// it): a strip at (u, v); `dx` x `dy` x `dz` the cube's size.
fn box_rects(u: f64, v: f64, dx: f64, dy: f64, dz: f64) -> [(f64, f64, f64, f64); 6] {
    let west = (u, v + dz, u + dz, v + dz + dy);
    let north = (u + dz, v + dz, u + dz + dx, v + dz + dy);
    let east = (u + dz + dx, v + dz, u + dz + dx + dz, v + dz + dy);
    let south = (
        u + dz + dx + dz,
        v + dz,
        u + 2.0 * dz + 2.0 * dx,
        v + dz + dy,
    );
    let up = (u + dz, v, u + dz + dx, v + dz);
    let down = (u + dz + dx, v, u + dz + dx + dx, v + dz);
    [north, east, south, west, up, down]
}

/// One baked face: four `[x, y, z, u, v]` vertices and the normal.
#[derive(Clone, Copy)]
struct Quad([[f64; 5]; 4], [f64; 3]);

/// The corner indexes of each face's four quad vertices, matching the game's
/// own exporter (`VanillaEntityModelExporter`), read off vanilla's baked head:
/// every face's first vertex pairs with the region's top-right corner, except
/// the mc-space bottom face, which pairs with its bottom-right.
const FACE_CORNERS: [[usize; 4]; 6] = [
    [3, 2, 0, 1], // top: D C A B
    [5, 4, 6, 7], // bottom: F E G H
    [4, 2, 6, 0], // west: A C G E
    [1, 0, 4, 5], // north: B A E F
    [3, 1, 5, 7], // east: D B F H
    [2, 6, 7, 3], // south: C G H D
];

/// Bakes one cube's six faces from (possibly rotated) corners and normals.
fn cube_faces(
    corners: &[[f64; 3]; 8],
    normals: &[[f64; 3]; 6],
    rects: &[(f64, f64, f64, f64); 6],
    atlas_w: f64,
    atlas_h: f64,
) -> [Quad; 6] {
    let uv = |r: (f64, f64, f64, f64)| {
        (
            r.0 / atlas_w,
            r.1 / atlas_h,
            r.2 / atlas_w,
            r.3 / atlas_h,
        )
    };
    // The rects arrive in game-face order (top, bottom, west, north, east,
    // south), already remapped by the caller.
    let uv_rects = std::array::from_fn::<_, 6, _>(|i| uv(rects[i]));
    let mut out = [Quad([[0.0; 5]; 4], [0.0; 3]); 6];
    for (face, quad) in out.iter_mut().enumerate() {
        let (u0, v0, u1, v1) = uv_rects[face];
        let corner_uvs = [(u1, v0), (u0, v0), (u0, v1), (u1, v1)];
        let verts: [[f64; 5]; 4] = std::array::from_fn(|i| {
            let c = corners[FACE_CORNERS[face][i]];
            let (u, v) = corner_uvs[i];
            [c[0], c[1], c[2], u, v]
        });
        *quad = Quad(verts, normals[face]);
    }
    out
}

/// Bones that never render. `background…` is YSM's preview backdrop; the
/// `Expression_*` / non-default `Mouth_*` bones are the model's alternate
/// face overlays, swapped by YSM's molang animations — we run none, so
/// keeping them would stack every variant z-fighting on the face (the
/// default mouth lives in `Mouth_chill`, kept).
fn junk_bone(bone: &BbBone) -> bool {
    let name = bone.name.to_ascii_lowercase();
    name.contains("background")
        || name.starts_with("expression")
        || (name.starts_with("mouth") && name != "mouth_chill")
        // YSM 的「同行宠物」狐狸是独立骨架（`FOX` → `AllBody2`），平时由
        // molang 变量 `v.roaming.a` 控制显隐。我们没有 molang 运行时，放它
        // 渲染就会和人形叠在一起，看起来像一堆乱方块，所以默认不烘焙。
        || name == "fox"
}

fn bake(
    bb: &BbModel,
    mut mapping: Mapping,
    texture_override: Option<usize>,
) -> Result<ConvertedPack, String> {
    // Stitch the textures vertically: each texture keeps its own pixel size;
    // its face uvs shift by the offset below.
    let mut atlas_w = 0u32;
    let mut atlas_h = 0u32;
    let mut offsets = Vec::new();
    for (w, h, _) in &bb.textures {
        offsets.push(f64::from(atlas_h));
        atlas_w = atlas_w.max(*w);
        atlas_h += h;
    }
    let mut rgba = vec![0u8; usize::try_from(u64::from(atlas_w) * u64::from(atlas_h) * 4).unwrap_or(0)];
    for (index, (w, h, pixels)) in bb.textures.iter().enumerate() {
        let y0 = offsets[index] as usize;
        for row in 0..*h as usize {
            let src = row * *w as usize * 4;
            let dst = (y0 + row) * atlas_w as usize * 4;
            rgba[dst..dst + *w as usize * 4].copy_from_slice(&pixels[src..src + *w as usize * 4]);
        }
    }
    let (atlas_w_f, atlas_h_f) = (f64::from(atlas_w), f64::from(atlas_h));

    // Per-cube uv rects in atlas pixels. A texture override (the YSM-style
    // material switch) forces every cube onto one texture.
    let rects: HashMap<String, [(f64, f64, f64, f64); 6]> = bb
        .cubes
        .iter()
        .map(|(uuid, cube)| {
            let tex = texture_override
                .unwrap_or(cube.texture.unwrap_or(0))
                .min(bb.textures.len() - 1);
            let (tw, th, _) = bb.textures[tex];
            let offset = offsets[tex];
            let scale = |r: [f64; 4]| -> (f64, f64, f64, f64) {
                (
                    r[0] / bb.uv_width * f64::from(tw),
                    offset + r[1] / bb.uv_height * f64::from(th),
                    r[2] / bb.uv_width * f64::from(tw),
                    offset + r[3] / bb.uv_height * f64::from(th),
                )
            };
            let dx = (cube.to[0] - cube.from[0]).abs();
            let dy = (cube.to[1] - cube.from[1]).abs();
            let dz = (cube.to[2] - cube.from[2]).abs();
            let (u, v) = (
                cube.uv_offset[0] / bb.uv_width * f64::from(tw),
                cube.uv_offset[1] / bb.uv_height * f64::from(th),
            );
            let box_uv = box_rects(u, v, dx, dy, dz);
            let mut out = [(0.0, 0.0, 0.0, 0.0); 6];
            for (i, slot) in out.iter_mut().enumerate() {
                *slot = cube.faces[i].map(scale).unwrap_or(box_uv[i]);
            }
            (uuid.clone(), out)
        })
        .collect();

    // Bones to parts, depth first — the same shape YSM's `GeoBone` uses:
    // `pose.xyz` is the pivot offset from the parent (so the parent's
    // rotation carries it), `pose.rgb` is the bone's own REST rotation in
    // radians, and the polygons are relative to the bone's own pivot with
    // only the cube's own rotation baked in.
    //
    // Baking the rest rotation into the vertices instead (what an earlier
    // version did) breaks any model whose bones are posed in the geometry:
    // the child pivot never rotated with its parent, so every bone with a
    // non-zero rest angle left its children behind — head, hands and feet
    // floated away from the body. It also double-rotated whenever an
    // animation supplied a rotation for the same bone.
    #[allow(clippy::too_many_arguments)]
    fn part(
        index: usize,
        bones: &[BbBone],
        cubes: &HashMap<String, BbCube>,
        rects: &HashMap<String, [(f64, f64, f64, f64); 6]>,
        mapping: Mapping,
        atlas: (f64, f64),
        parent_pivot: [f64; 3],
        names: &mut HashMap<usize, String>,
        used: &mut std::collections::HashSet<String>,
    ) -> Value {
        let bone = &bones[index];
        let flip = |x: f64| if mapping.flip_x { -x } else { x };
        let mapped_pivot = [
            flip(bone.pivot[0]),
            mapping.mc_height - bone.pivot[1],
            bone.pivot[2],
        ];
        // The mapping mirrors x AND turns y upside down: its linear part is
        // diag(-1, -1, 1), a 180° turn about z (handedness preserved). A bone
        // rest rotation R conjugates to D·R·D⁻¹, whose Euler ZYX angles are
        // (-x, -y, +z): x and y reverse, z stays. Taking x as-is and z
        // negated (an x-plane *reflection*'s rule) flipped every bone that
        // leans about x or z — the fox tail folded down through the legs
        // instead of rising behind the back, and arms posed by a z lean
        // rotated into the torso and vanished.
        let rest_rot = if mapping.flip_x {
            [
                -bone.rotation[0].to_radians(),
                -bone.rotation[1].to_radians(),
                bone.rotation[2].to_radians(),
            ]
        } else {
            bone.rotation.map(f64::to_radians)
        };
        let mut node = Map::new();
        node.insert(
            "pose".into(),
            json!([
                mapped_pivot[0] - parent_pivot[0],
                mapped_pivot[1] - parent_pivot[1],
                mapped_pivot[2] - parent_pivot[2],
                rest_rot[0],
                rest_rot[1],
                rest_rot[2],
                1.0,
                1.0,
                1.0
            ]),
        );
        // An Euler rotation around a pivot, degrees. Blockbench (and YSM's
        // `MathUtil.getQuatFromEulerZYX`) uses the ZYX order, R = Rz * Ry * Rx;
        // an XYZ order turns every multi-axis rest rotation (ears, mask, tail)
        // inside out, which reads as a broken model even though the geometry
        // is fine.
        fn rotate(v: [f64; 3], euler_deg: [f64; 3], pivot: [f64; 3]) -> [f64; 3] {
            let rad = euler_deg.map(f64::to_radians);
            let (sx, cx) = rad[0].sin_cos();
            let (sy, cy) = rad[1].sin_cos();
            let (sz, cz) = rad[2].sin_cos();
            let p = [v[0] - pivot[0], v[1] - pivot[1], v[2] - pivot[2]];
            // Rightmost factor first: X, then Y, then Z.
            let (x1, y1, z1) = (p[0], cx * p[1] - sx * p[2], sx * p[1] + cx * p[2]);
            let (x2, y2, z2) = (cy * x1 + sy * z1, y1, -sy * x1 + cy * z1);
            let (x3, y3) = (cz * x2 - sz * y2, sz * x2 + cz * y2);
            [x3 + pivot[0], y3 + pivot[1], z2 + pivot[2]]
        }
        let mut polygons = Vec::new();
        let mut normals = Vec::new();
        let mut children = Map::new();
        for child in &bone.children {
            match child {
                Child::Cube(uuid) => {
                    let Some(cube) = cubes.get(uuid) else {
                        continue;
                    };
                    if !cube.visible {
                        continue;
                    }
                    // A 1×1×1 cube sitting exactly on its bone's pivot is a
                    // hand/waist attachment marker (Blockbench has no locator
                    // element, so YSM exports one as a hair cube): it is
                    // meant to be invisible, and drawing it puts a single
                    // dark pixel beside each hand.
                    let size = [
                        (cube.to[0] - cube.from[0]).abs(),
                        (cube.to[1] - cube.from[1]).abs(),
                        (cube.to[2] - cube.from[2]).abs(),
                    ];
                    let cube_center = [
                        (cube.from[0] + cube.to[0]) * 0.5,
                        (cube.from[1] + cube.to[1]) * 0.5,
                        (cube.from[2] + cube.to[2]) * 0.5,
                    ];
                    if size.iter().all(|s| *s <= 1.01)
                        && (0..3).all(|i| (cube_center[i] - bone.pivot[i]).abs() < 0.01)
                    {
                        continue;
                    }
                    let rect = &rects[uuid];
                    // bbmodel-space face normals and uv rects, both in the
                    // order north, east, south, west, up, down.
                    let bb_normals: [[f64; 3]; 6] = [
                        [0.0, 0.0, -1.0],
                        [1.0, 0.0, 0.0],
                        [0.0, 0.0, 1.0],
                        [-1.0, 0.0, 0.0],
                        [0.0, 1.0, 0.0],
                        [0.0, -1.0, 0.0],
                    ];
                    let bb_normals_rot = if let Some((euler, _pivot)) = cube.rotation {
                        bb_normals.map(|n| rotate(n, euler, [0.0; 3]))
                    } else {
                        bb_normals
                    };
                    // Map a bbmodel-space point or normal into the game's
                    // model space (mirror x, y down).
                    let map_p = |p: [f64; 3]| -> [f64; 3] {
                        [flip(p[0]), mapping.mc_height - p[1], p[2]]
                    };
                    let map_n = |n: [f64; 3]| -> [f64; 3] { [flip(n[0]), -n[1], n[2]] };
                    // The cube's corners in bbmodel space, indexed like
                    // `FACE_CORNERS` (game small y = bbmodel large y; with
                    // flip_x the game small x side is the bbmodel large x).
                    let (x0, x1) = if mapping.flip_x {
                        (cube.to[0], cube.from[0])
                    } else {
                        (cube.from[0], cube.to[0])
                    };
                    let (y0, y1) = (cube.to[1], cube.from[1]);
                    let (z0, z1) = (cube.from[2], cube.to[2]);
                    let mut corners: [[f64; 3]; 8] = [
                        [x0, y0, z0],
                        [x1, y0, z0],
                        [x0, y0, z1],
                        [x1, y0, z1],
                        [x0, y1, z0],
                        [x1, y1, z0],
                        [x0, y1, z1],
                        [x1, y1, z1],
                    ];
                    if let Some((euler, pivot)) = cube.rotation {
                        for c in &mut corners {
                            *c = rotate(*c, euler, pivot);
                        }
                    }
                    // Game-space face order: top, bottom, west, north, east,
                    // south. Mirroring x swaps the bbmodel's east/west sides.
                    let (west_src, east_src) = if mapping.flip_x { (1, 3) } else { (3, 1) };
                    let face_normals = [
                        map_n(bb_normals_rot[4]),
                        map_n(bb_normals_rot[5]),
                        map_n(bb_normals_rot[west_src]),
                        map_n(bb_normals_rot[0]),
                        map_n(bb_normals_rot[east_src]),
                        map_n(bb_normals_rot[2]),
                    ];
                    let face_rects = [
                        rect[4], rect[5], rect[west_src], rect[0], rect[east_src], rect[2],
                    ];
                    // Pivot-relative vertices: the game (and the preview)
                    // translate by the accumulated pose before rendering.
                    let mapped: [[f64; 3]; 8] = corners.map(map_p).map(|p| {
                        [
                            p[0] - mapped_pivot[0],
                            p[1] - mapped_pivot[1],
                            p[2] - mapped_pivot[2],
                        ]
                    });
                    // YSM models park never-shown decoration cubes (sparkles,
                    // splash marks) far off the body, hidden by their molang
                    // animations which we don't run. A cube whose world
                    // center sits far outside the body's envelope would
                    // float beside the player forever: drop it. Everything
                    // legit (body, hair, ears, back props) stays within ~31
                    // units of the body center; the junk sits at ~49.
                    let center = mapped.iter().fold([0.0; 3], |c, p| {
                        [c[0] + p[0], c[1] + p[1], c[2] + p[2]]
                    });
                    let center = [
                        mapped_pivot[0] + center[0] / 8.0,
                        mapped_pivot[1] + center[1] / 8.0,
                        mapped_pivot[2] + center[2] / 8.0,
                    ];
                    let anchor = mapping.mc_height * 0.5;
                    let away = (center[0] * center[0]
                        + (center[1] - anchor) * (center[1] - anchor)
                        + center[2] * center[2])
                        .sqrt();
                    if away > 36.0 {
                        continue;
                    }
                    let quads = cube_faces(
                        &mapped,
                        &face_normals,
                        &face_rects,
                        atlas.0,
                        atlas.1,
                    );
                    for quad in quads {
                        let Quad(verts, normal) = quad;
                        polygons.push(json!(verts.map(|v| [v[0], v[1], v[2], v[3], v[4]])));
                        normals.push(json!(normal));
                    }
                }
                Child::Bone(kid) => {
                    if junk_bone(&bones[*kid]) || !bones[*kid].visible {
                        continue;
                    }
                    let kid_name = part_name(&bones[*kid].name)
                        .map_or_else(|| format!("{}_{}", bones[*kid].name.to_ascii_lowercase(), kid), str::to_owned);
                    // Names are the key into the engine's pose map, which is
                    // flat: uniqueness has to hold across the whole tree, not
                    // just this bone's siblings.
                    let kid_name = global_unique(kid_name, used);
                    names.insert(*kid, kid_name.clone());
                    children.insert(
                        kid_name,
                        part(
                            *kid,
                            bones,
                            cubes,
                            rects,
                            mapping,
                            atlas,
                            mapped_pivot,
                            names,
                            used,
                        ),
                    );
                }
            }
        }
        if !polygons.is_empty() {
            node.insert("polygons".into(), Value::Array(polygons));
            node.insert("normals".into(), Value::Array(normals));
        }
        if !children.is_empty() {
            node.insert("children".into(), Value::Object(children));
        }
        Value::Object(node)
    }
    fn global_unique(name: String, used: &mut std::collections::HashSet<String>) -> String {
        if used.insert(name.clone()) {
            return name;
        }
        for n in 2.. {
            let candidate = format!("{name}_{n}");
            if used.insert(candidate.clone()) {
                return candidate;
            }
        }
        name
    }
    // YSM models keep every prop the avatar can ever hold as a SEPARATE
    // top-level bone (a bow, a shield, a health bottle, the mod's logo),
    // shown by molang and parked far outside the body. We run no molang, so
    // an unfiltered bake draws a sword, a bottle and a giant "CE" beside the
    // player. Keep only the roots whose envelope touches the body's.
    let span = |index: usize| -> Option<([f64; 3], [f64; 3], usize)> {
        fn walk(
            index: usize,
            bones: &[BbBone],
            cubes: &HashMap<String, BbCube>,
            lo: &mut [f64; 3],
            hi: &mut [f64; 3],
            quads: &mut usize,
        ) {
            for child in &bones[index].children {
                match child {
                    Child::Cube(uuid) => {
                        if let Some(cube) = cubes.get(uuid) {
                            *quads += 6;
                            for i in 0..3 {
                                lo[i] = lo[i].min(cube.from[i].min(cube.to[i]));
                                hi[i] = hi[i].max(cube.from[i].max(cube.to[i]));
                            }
                        }
                    }
                    Child::Bone(kid) => walk(*kid, bones, cubes, lo, hi, quads),
                }
            }
        }
        let mut lo = [f64::MAX; 3];
        let mut hi = [f64::MIN; 3];
        let mut quads = 0usize;
        walk(index, &bb.bones, &bb.cubes, &mut lo, &mut hi, &mut quads);
        (quads > 0 && lo[0] <= hi[0]).then_some((lo, hi, quads))
    };
    let spans: Vec<Option<([f64; 3], [f64; 3], usize)>> =
        bb.roots.iter().map(|index| span(*index)).collect();
    let main = spans
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_some())
        .max_by_key(|(_, s)| s.map_or(0, |(_, _, q)| q))
        .map(|(i, _)| i);
    let keep_root = |position: usize| -> bool {
        let Some(main) = main else {
            return true;
        };
        if position == main {
            return true;
        }
        let (Some((lo, hi, _)), Some((mlo, mhi, _))) = (spans[position], spans[main]) else {
            // A root with no geometry is a pure bone chain (the head, the
            // skirt): its children write into the same tree, keep it.
            return true;
        };
        // A worn piece (wing, cape, tail) shares the body's envelope; every
        // prop YSM parks for a molang to show sits a body-width away. A root
        // named like a human bone is the body itself, never a prop — a wide
        // model's arms used to sit just past the distance limit and were
        // dropped ("the arms vanished"). The rest is judged on the envelope
        // centres, which keeps hand-held pieces that touch the body box.
        if part_name(&bb.bones[bb.roots[position]].name).is_some() {
            return true;
        }
        let centre = |lo: [f64; 3], hi: [f64; 3]| {
            [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5]
        };
        let (c, mc) = (centre(lo, hi), centre(mlo, mhi));
        let d = [c[0] - mc[0], c[1] - mc[1], c[2] - mc[2]];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() <= 12.0
    };
    // Keep the pack's soles on the ground: `mc_height` is where the game's
    // y=0 sits in bbmodel units, and a pack rarely ends exactly on 0 (a boot
    // sole at y=0.1, a cape hanging to -3). Without this the whole model
    // floats or sinks by that offset — the "feet slid up" look.
    if let Some(main) = main
        && let Some((lo, _, _)) = spans[main]
    {
        mapping.mc_height = mapping.mc_height.min(24.0 + lo[1]);
    }
    // The root part holds the outliner's top-level bones, named by the
    // vanilla skeleton where they match.
    let mut root_children = Map::new();
    let mut names: HashMap<usize, String> = HashMap::new();
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    used.insert("root".to_owned());
    for (position, index) in bb.roots.iter().enumerate() {
        if junk_bone(&bb.bones[*index]) || !bb.bones[*index].visible || !keep_root(position) {
            continue;
        }
        let bone = &bb.bones[*index];
        let name = part_name(&bone.name)
            .map_or_else(|| format!("{}_{}", bone.name.to_ascii_lowercase(), index), str::to_owned);
        let name = if name.is_empty() || name.starts_with('_') {
            format!("part_{position}")
        } else {
            name
        };
        let name = global_unique(name, &mut used);
        names.insert(*index, name.clone());
        root_children.insert(
            name,
            part(
                *index,
                &bb.bones,
                &bb.cubes,
                &rects,
                mapping,
                (atlas_w_f, atlas_h_f),
                [0.0; 3],
                &mut names,
                &mut used,
            ),
        );
    }
    let model = json!({
        "pose": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        "children": Value::Object(root_children),
    });
    let animations = bake_animations(&bb.animations, bb, &names);
    Ok(ConvertedPack {
        model,
        texture: (atlas_w, atlas_h, rgba),
        animations,
    })
}

/// Animation names never shipped: the TAC / Carry On mod integrations, the
/// vehicle addons, the model-select GUI pose and the head-look layer (the
/// game's own head tracking replaces it).
fn skipped_animation(name: &str) -> bool {
    name.starts_with("tac:")
        || name.starts_with("carryon:")
        || name.starts_with("vehicle$")
        || name.starts_with("passenger$")
        || name == "keyboard"
        || name == "gui"
        || name.starts_with("head:")
}

/// Parses one bbmodel keyframe value: a plain number, or a molang expression
/// (particles, head look, food level...) we don't evaluate — those sample 0.
fn molang_number(v: &Value) -> f64 {
    v.as_str()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .or_else(|| v.as_f64())
        .unwrap_or(0.0)
}

/// Converts the bbmodel's Blockbench animations into the game's keyframe
/// schema: `{length, looping, bones: {part: [{target, keyframes}]}}`.
/// Keyframe values arrive as degree strings; the game's model space mirrors
/// bbmodel space by a 180° turn about z (x and y negated, z kept), so
/// rotations become `(-x, -y, z)` radians and positions `(-x, -y, z)` units.
/// One bedrock channel value → `[x, y, z]`: a vector, or a scalar broadcast
/// to all three axes (a plain `"scale": 1.3`). Molang expressions give None.
fn bedrock_vec3(v: &Value) -> Option<[f64; 3]> {
    match v {
        Value::Array(a) if a.len() == 3 => {
            Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?])
        }
        Value::Number(n) => {
            let x = n.as_f64()?;
            Some([x, x, x])
        }
        _ => None,
    }
}

/// The frames of one bedrock channel: a time map `{"0.5": [0, 10, 0]}` or a
/// constant. Each frame carries `(time, post, pre, interpolation)`; the
/// value may be a bare vector or `{"pre": …, "post": …, "lerp_mode": …}`.
fn bedrock_channel(c: &Value) -> Vec<(f64, [f64; 3], Option<[f64; 3]>, &'static str)> {
    let mut out = Vec::new();
    match c {
        Value::Object(map) => {
            for (time, raw) in map {
                let Ok(time) = time.trim().parse::<f64>() else {
                    continue; // a molang-time keyframe
                };
                let (post, pre, mode) = match raw {
                    Value::Object(o) => {
                        let post = o
                            .get("post")
                            .and_then(bedrock_vec3)
                            .or_else(|| o.get("pre").and_then(bedrock_vec3));
                        let pre = o.get("pre").and_then(bedrock_vec3);
                        let mode = match o.get("lerp_mode").and_then(Value::as_str) {
                            Some("catmullrom") => "catmullrom",
                            Some("step") => "step",
                            _ => "linear",
                        };
                        (post, pre, mode)
                    }
                    other => (bedrock_vec3(other), None, "linear"),
                };
                let Some(post) = post else {
                    continue; // a molang value: we don't evaluate it
                };
                out.push((time, post, pre, mode));
            }
        }
        other => {
            if let Some(v) = bedrock_vec3(other) {
                out.push((0.0, v, None, "linear"));
            }
        }
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

/// A bedrock animation file (`animations/*.json`) → the Blockbench
/// `animations` array `bake_animations` consumes. Bones are addressed by
/// NAME: a ysm package has no uuids, and its `by_uuid` table maps names to
/// bone indexes. Group headers (`"——物理模拟——"`) carry no bones and fall
/// out on their own.
fn bedrock_animations(file: &Value) -> Value {
    let mut out = Vec::new();
    for (name, anim) in file["animations"].as_object().into_iter().flatten() {
        let mut animators = Map::new();
        let mut latest = 0.0f64;
        for (bone, channels) in anim["bones"].as_object().into_iter().flatten() {
            let mut keyframes = Vec::new();
            for channel in ["rotation", "position", "scale"] {
                let Some(c) = channels.get(channel) else {
                    continue;
                };
                for (time, post, pre, mode) in bedrock_channel(c) {
                    latest = latest.max(time);
                    let mut keyframe = json!({
                        "channel": channel,
                        "time": time,
                        "data_points": [{"x": post[0], "y": post[1], "z": post[2]}],
                        "interpolation": mode,
                    });
                    if let Some(pre) = pre {
                        keyframe["pre"] = json!({"x": pre[0], "y": pre[1], "z": pre[2]});
                    }
                    keyframes.push(keyframe);
                }
            }
            if !keyframes.is_empty() {
                animators.insert(bone.clone(), json!({ "keyframes": keyframes }));
            }
        }
        if animators.is_empty() {
            continue;
        }
        let length = anim["animation_length"].as_f64().unwrap_or(latest);
        let looping = match &anim["loop"] {
            Value::Bool(true) => "loop",
            Value::String(s) if s == "hold" => "hold",
            _ => "once",
        };
        out.push(json!({
            "name": name,
            "length": length,
            "loop": looping,
            "animators": animators,
        }));
    }
    Value::Array(out)
}

fn bake_animations(raw: &Value, bb: &BbModel, names: &HashMap<usize, String>) -> Value {
    let mut out = Map::new();
    for anim in raw.as_array().into_iter().flatten() {
        let name = anim["name"].as_str().unwrap_or_default();
        if name.is_empty() || skipped_animation(name) {
            continue;
        }
        let length = anim["length"].as_f64().unwrap_or(0.0);
        // "loop" repeats, "hold" plays once and freezes on the last frame,
        // "once" plays once and stops (both clamp; looping only for "loop").
        let looping = anim["loop"].as_str() == Some("loop");
        let mut bones = Map::new();
        for (uuid, animator) in anim["animators"].as_object().into_iter().flatten() {
            let Some(index) = bb.by_uuid.get(uuid) else {
                continue;
            };
            let Some(part) = names.get(index) else {
                continue; // a junk bone (backdrop, spare expressions)
            };
            let mut channels = Vec::new();
            for (target, channel) in [
                ("rotation", "rotation"),
                ("position", "position"),
                ("scale", "scale"),
            ] {
                let mut keyframes: Vec<(f64, [f64; 3], [f64; 3], bool)> = Vec::new();
                for kf in animator["keyframes"].as_array().into_iter().flatten() {
                    if kf["channel"].as_str() != Some(channel) {
                        continue;
                    }
                    let time = kf["time"].as_f64().unwrap_or(0.0);
                    // Molang expressions we can't evaluate: a rotation or
                    // offset of 0 is the bone's rest pose (already baked into
                    // the vertices), but a scale of 0 would erase the part, so
                    // scale keyframes with a non-numeric axis are dropped.
                    let numeric = |dp: &Value| -> bool {
                        ["x", "y", "z"].iter().all(|k| {
                            let v = &dp[*k];
                            v.as_f64().is_some()
                                || v.as_str().is_some_and(|s| s.trim().parse::<f64>().is_ok())
                        })
                    };
                    if channel == "scale" && !numeric(&kf["data_points"][0]) {
                        continue;
                    }
                    let read = |dp: &Value| -> [f64; 3] {
                        let v = [
                            molang_number(&dp["x"]),
                            molang_number(&dp["y"]),
                            molang_number(&dp["z"]),
                        ];
                        match channel {
                            // Degrees to radians, mirrored into the game's
                            // model space (the 180° z turn conjugation).
                            "rotation" => [
                                -v[0].to_radians(),
                                -v[1].to_radians(),
                                v[2].to_radians(),
                            ],
                            "position" => [-v[0], -v[1], v[2]],
                            _ => v,
                        }
                    };
                    let post = read(&kf["data_points"][0]);
                    let pre = kf["pre"].as_object().map(|_| read(&kf["pre"])).unwrap_or(post);
                    let catmull = kf["interpolation"].as_str() == Some("catmullrom");
                    // A step holds the previous value: express as the previous
                    // post extended to this keyframe's time (pre = post of the
                    // value, i.e. no change across the segment is impossible;
                    // vanilla's sample lerps pre->post, so freeze at `pre`).
                    let (pre, post) = if kf["interpolation"].as_str() == Some("step") {
                        (pre, pre)
                    } else {
                        (pre, post)
                    };
                    keyframes.push((time, pre, post, catmull));
                }
                if keyframes.is_empty() {
                    continue;
                }
                keyframes.sort_by(|a, b| a.0.total_cmp(&b.0));
                let keyframes: Vec<Value> = keyframes
                    .into_iter()
                    .map(|(t, pre, post, catmull)| {
                        json!([
                            t,
                            pre,
                            post,
                            if catmull { "catmullrom" } else { "linear" }
                        ])
                    })
                    .collect();
                channels.push(json!({
                    "target": target,
                    "keyframes": keyframes,
                }));
            }
            if !channels.is_empty() {
                bones.insert(part.clone(), Value::Array(channels));
            }
        }
        if !bones.is_empty() {
            out.insert(
                name.to_owned(),
                json!({"length": length, "looping": looping, "bones": bones}),
            );
        }
    }
    if out.is_empty() {
        Value::Null
    } else {
        Value::Object(out)
    }
}

/// Converts a Yes Steve Model `.ysm` package (a zip with `ysm.json`, bedrock
/// geometry `models/*.json`, bedrock animations and textures) into the game's
/// model schema. `texture_index` picks one of the package's textures as the
/// material (none = the first), which is how texture switching re-converts.
pub fn convert_ysm(
    bytes: &[u8],
    texture_index: Option<usize>,
) -> Result<ConvertedPack, String> {
    use std::io::Read as _;
    let err = |what: String| format!("ysm: {what}");
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| err(e.to_string()))?;
    let manifest: Value = {
        let mut raw = Vec::new();
        archive
            .by_name("ysm.json")
            .map_err(|e| err(format!("ysm.json: {e}")))?
            .read_to_end(&mut raw)
            .map_err(|e| err(e.to_string()))?;
        serde_json::from_slice(&raw)
    }
    .map_err(|e| err(format!("ysm.json JSON: {e}")))?;
    let files = &manifest["files"]["player"];
    let model_path = files["model"]["main"]
        .as_str()
        .ok_or_else(|| err("ysm.json has no player model entry".into()))?;
    let geometry: Value = {
        let mut raw = Vec::new();
        archive
            .by_name(model_path)
            .map_err(|e| err(format!("{model_path}: {e}")))?
            .read_to_end(&mut raw)
            .map_err(|e| err(e.to_string()))?;
        serde_json::from_slice(&raw)
    }
    .map_err(|e| err(format!("{model_path} JSON: {e}")))?;
    // Bedrock geometry: either {"minecraft:geometry": [...]} or a bare list.
    let geometries = geometry["minecraft:geometry"]
        .as_array()
        .or_else(|| geometry.as_array())
        .ok_or_else(|| err(format!("{model_path} has no minecraft:geometry")))?;
    let bones_json = geometries
        .first()
        .and_then(|g| g["bones"].as_array())
        .ok_or_else(|| err(format!("{model_path} has no bones")))?;

    // The package's textures become the atlas; uv rects are pixels of the
    // selected one (bedrock uvs read one texture image, the material list is
    // for switching, not blending).
    let texture_paths: Vec<String> = files["texture"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|t| t.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if texture_paths.is_empty() {
        return Err(err("ysm.json has no textures".into()));
    }
    let selected = texture_index.unwrap_or(0);
    if selected >= texture_paths.len() {
        return Err(format!(
            "ysm texture {selected} out of range (has {})",
            texture_paths.len()
        ));
    }
    let (tex_w, tex_h, rgba) = {
        let mut raw = Vec::new();
        archive
            .by_name(&texture_paths[selected])
            .map_err(|e| err(format!("{}: {e}", texture_paths[selected])))?
            .read_to_end(&mut raw)
            .map_err(|e| err(e.to_string()))?;
        decode_png(&raw)?
    };

    // Transcode the bedrock bone tree into the bbmodel-shaped structures the
    // baker already understands. Bedrock rotates around x and z with the
    // opposite sign of Blockbench's display, so they are negated here.
    struct BbNode {
        name: String,
        pivot: [f64; 3],
        rotation: [f64; 3],
        cubes: Vec<(String, BbCube)>,
        parent: Option<String>,
    }
    let mut nodes: Vec<BbNode> = Vec::new();
    let mut index_of: HashMap<String, usize> = HashMap::new();
    for b in bones_json {
        let name = b["name"].as_str().unwrap_or_default().to_owned();
        if name.is_empty() {
            continue;
        }
        let pivot = [
            b["pivot"][0].as_f64().unwrap_or(0.0),
            b["pivot"][1].as_f64().unwrap_or(0.0),
            b["pivot"][2].as_f64().unwrap_or(0.0),
        ];
        let rotation = [
            -b["rotation"][0].as_f64().unwrap_or(0.0),
            b["rotation"][1].as_f64().unwrap_or(0.0),
            -b["rotation"][2].as_f64().unwrap_or(0.0),
        ];
        let mut cubes = Vec::new();
        for (ci, c) in b["cubes"].as_array().into_iter().flatten().enumerate() {
            let uuid = format!("{name}/{ci}");
            let origin: [f64; 3] = [
                c["origin"][0].as_f64().unwrap_or(0.0),
                c["origin"][1].as_f64().unwrap_or(0.0),
                c["origin"][2].as_f64().unwrap_or(0.0),
            ];
            let size: [f64; 3] = [
                c["size"][0].as_f64().unwrap_or(0.0),
                c["size"][1].as_f64().unwrap_or(0.0),
                c["size"][2].as_f64().unwrap_or(0.0),
            ];
            // Bedrock uv: either a box offset [u, v] or per-face
            // {"uv": [u, v], "uv_size": [w, h]} (negative sizes mirror).
            let face_rect = |face: &Value| -> Option<[f64; 4]> {
                let (u, v) = (
                    face["uv"][0].as_f64()?,
                    face["uv"][1].as_f64()?,
                );
                let (w, h) = (
                    face["uv_size"][0].as_f64().unwrap_or(0.0),
                    face["uv_size"][1].as_f64().unwrap_or(0.0),
                );
                Some([
                    u.min(u + w),
                    v.min(v + h),
                    u.max(u + w),
                    v.max(v + h),
                ])
            };
            let faces = [
                face_rect(&c["uv"]["north"]),
                face_rect(&c["uv"]["east"]),
                face_rect(&c["uv"]["south"]),
                face_rect(&c["uv"]["west"]),
                face_rect(&c["uv"]["up"]),
                face_rect(&c["uv"]["down"]),
            ];
            let (dx, dy, dz) = (size[0], size[1], size[2]);
            let box_offset = c["uv"].as_array().and_then(|uv| {
                Some((
                    uv[0].as_f64().unwrap_or(0.0),
                    uv[1].as_f64().unwrap_or(0.0),
                ))
            });
            let cube_rotation = (c["rotation"][0].as_f64().is_some()).then(|| {
                (
                    [
                        -c["rotation"][0].as_f64().unwrap_or(0.0),
                        c["rotation"][1].as_f64().unwrap_or(0.0),
                        -c["rotation"][2].as_f64().unwrap_or(0.0),
                    ],
                    [
                        c["pivot"][0].as_f64().unwrap_or(0.0),
                        c["pivot"][1].as_f64().unwrap_or(0.0),
                        c["pivot"][2].as_f64().unwrap_or(0.0),
                    ],
                )
            });
            cubes.push((
                uuid.clone(),
                BbCube {
                    from: origin,
                    to: [origin[0] + dx, origin[1] + dy, origin[2] + dz],
                    faces,
                    box_uv: false,
                    uv_offset: box_offset.unwrap_or((0.0, 0.0)).into(),
                    texture: None,
                    rotation: cube_rotation,
                    visible: true,
                },
            ));
            // The baker's box-unwrap fallback (below) reads `uv_offset`.
        }
        nodes.push(BbNode {
            name,
            pivot,
            rotation,
            cubes,
            parent: b["parent"].as_str().map(str::to_owned),
        });
        index_of.insert(nodes.last().unwrap().name.clone(), nodes.len() - 1);
    }
    // The package's animations: `files.player.animation` names the bedrock
    // animation files (main, extra, ...). Merge them all — this package's
    // rocks are addressed by name from `properties.extra_animation`, and the
    // wheel / state machine play them by name. Skipping this left every
    // `.ysm` pack frozen in its rest pose with an empty action list.
    let animation_paths: Vec<String> = match &files["animation"] {
        Value::Object(map) => map
            .values()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Value::Array(list) => list
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    };
    let mut animation_list = Vec::new();
    for path in &animation_paths {
        let Ok(mut entry) = archive.by_name(path) else {
            continue;
        };
        let mut raw = Vec::new();
        if entry.read_to_end(&mut raw).is_err() {
            continue;
        }
        drop(entry);
        if let Ok(v) = serde_json::from_slice::<Value>(&raw)
            && let Value::Array(list) = bedrock_animations(&v)
        {
            animation_list.extend(list);
        }
    }
    // The baker's uv layout: face rects are already texture pixels, so the
    // "uv canvas" is the texture itself.
    let mut cubes = HashMap::new();
    let mut bones = Vec::new();
    let mut roots = Vec::new();
    // Depth-first from parent-less bones so parents get indexes first.
    fn build(
        node_idx: usize,
        nodes: &[BbNode],
        index_of: &HashMap<String, usize>,
        cubes: &mut HashMap<String, BbCube>,
        bones: &mut Vec<BbBone>,
    ) -> Option<usize> {
        let node = &nodes[node_idx];
        if bones.len() > 4096 {
            return None; // cycle guard
        }
        let mut children = Vec::new();
        for (uuid, cube) in &node.cubes {
            let faceless = cube.faces.iter().all(|f| f.is_none());
            let mut cube = (*cube).clone();
            if faceless {
                // A box unwrap: the baker derives rects from uv_offset.
                cube.faces = [None; 6];
            }
            cubes.insert(uuid.clone(), cube);
            children.push(Child::Cube(uuid.clone()));
        }
        let index = bones.len();
        bones.push(BbBone {
            name: node.name.clone(),
            pivot: node.pivot,
            rotation: node.rotation,
            visible: true,
            children,
        });
        for (kid_idx, kid) in nodes.iter().enumerate() {
            if kid.parent.as_deref() == Some(node.name.as_str()) {
                if let Some(kid_bone) = build(kid_idx, nodes, index_of, cubes, bones) {
                    bones[index].children.push(Child::Bone(kid_bone));
                }
            }
        }
        Some(index)
    }
    for (idx, node) in nodes.iter().enumerate() {
        let is_root = node.parent.as_deref().is_none_or(|p| !index_of.contains_key(p));
        if is_root {
            if let Some(bone) = build(idx, &nodes, &index_of, &mut cubes, &mut bones) {
                roots.push(bone);
            }
        }
    }
    let bb = BbModel {
        cubes,
        bones,
        roots,
        by_uuid: index_of,
        animations: Value::Array(animation_list),
        textures: vec![(tex_w, tex_h, rgba)],
        uv_width: f64::from(tex_w),
        uv_height: f64::from(tex_h),
    };
    bake(&bb, Mapping::default(), texture_index)
}

/// The model's display name: `.ysm` reads `ysm.json`'s metadata, a bbmodel
/// reads its `name` field.
pub fn peek_model_name(bytes: &[u8]) -> Option<String> {
    use std::io::Read as _;
    if bytes.starts_with(b"\x89PNG") {
        return None; // a PNG is not a model
    }
    if let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) {
        let mut raw = Vec::new();
        archive
            .by_name("ysm.json")
            .ok()?
            .read_to_end(&mut raw)
            .ok()?;
        let manifest: Value = serde_json::from_slice(&raw).ok()?;
        return manifest["metadata"]["name"]
            .as_str()
            .map(str::to_owned);
    }
    let v: Value = serde_json::from_slice(bytes).ok()?;
    v["name"].as_str().map(str::to_owned)
}

/// The action buttons a YSM package declares for its roulette:
/// `properties.extra_animation` maps an animation name to the label shown on
/// the wheel, and each `extra_animation_classify` entry contributes its own
/// buttons (a `#name` key in the top menu opens that group). Menu keys start
/// with `#` and are not animations, so they are dropped. Returns
/// `(animation name, label)`; empty for a non-YSM file.
pub fn ysm_actions(bytes: &[u8]) -> Vec<(String, String)> {
    use std::io::Read as _;
    let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        return Vec::new();
    };
    let mut raw = Vec::new();
    let Ok(mut file) = archive.by_name("ysm.json") else {
        return Vec::new();
    };
    if file.read_to_end(&mut raw).is_err() {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_slice::<Value>(&raw) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let push = |group: &Value, out: &mut Vec<(String, String)>| {
        for (key, label) in group.as_object().into_iter().flatten() {
            if key.starts_with('#') {
                continue;
            }
            out.push((
                key.clone(),
                label.as_str().unwrap_or(key).to_owned(),
            ));
        }
    };
    push(&v["properties"]["extra_animation"], &mut out);
    for group in v["properties"]["extra_animation_classify"]
        .as_array()
        .into_iter()
        .flatten()
    {
        push(&group["extra_animation"], &mut out);
    }
    out
}

/// Renders the converted model to a preview image: an orthographic view,
/// z-buffered with lambert shading and nearest texture sampling. Both camera
/// sides are tried (models face either way) and the brighter render wins.
/// Returns (width, height, RGBA rows) at `size` x `size` pixels.
pub fn render_preview(pack: &ConvertedPack, size: u32) -> (u32, u32, Vec<u8>) {
    struct Face {
        verts: [[f64; 5]; 4],
        normal: [f64; 3],
    }
    let mut faces = Vec::new();
    // Compose the same hierarchy the game's `render_model` builds:
    // `parent * T(pose.xyz) * R(pose.rgb) * S(pose.scale)`. A child's pivot
    // must ride its parent's rotation — adding the parent pivot without the
    // parent rotation leaves every bone with a posed ancestor behind (the
    // "detached head" look), so the preview keeps the running basis.
    fn mat_mul(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
        let mut out = [[0.0; 3]; 3];
        for (i, row) in out.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
            }
        }
        out
    }
    fn mat_apply(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
        [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
    }
    fn basis(rad: [f64; 3]) -> [[f64; 3]; 3] {
        let (sx, cx) = rad[0].sin_cos();
        let (sy, cy) = rad[1].sin_cos();
        let (sz, cz) = rad[2].sin_cos();
        let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
        let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
        let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
        mat_mul(mat_mul(rz, ry), rx)
    }
    const IDENTITY: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    fn visit(
        node: &Value,
        parent_t: [f64; 3],
        parent_r: [[f64; 3]; 3],
        out: &mut Vec<Face>,
    ) {
        let pose = |i: usize| {
            node["pose"][i].as_f64().unwrap_or(match i {
                6..=8 => 1.0,
                _ => 0.0,
            })
        };
        let offset = mat_apply(parent_r, [pose(0), pose(1), pose(2)]);
        let pivot = [
            parent_t[0] + offset[0],
            parent_t[1] + offset[1],
            parent_t[2] + offset[2],
        ];
        let rot = mat_mul(parent_r, basis([pose(3), pose(4), pose(5)]));
        let scale = [pose(6), pose(7), pose(8)];
        let local = |v: [f64; 3]| -> [f64; 3] {
            let r = mat_apply(rot, [v[0] * scale[0], v[1] * scale[1], v[2] * scale[2]]);
            [pivot[0] + r[0], pivot[1] + r[1], pivot[2] + r[2]]
        };
        let polys: &[Value] = node["polygons"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let norms: &[Value] = node["normals"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        for (poly, normal) in polys.iter().zip(norms) {
            let verts: Vec<[f64; 5]> = poly
                .as_array()
                .map(|vs| {
                    vs.iter()
                        .map(|v| {
                            let a: Vec<f64> = v
                                .as_array()
                                .map(|c| c.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect())
                                .unwrap_or_default();
                            let world = local([a[0], a[1], a[2]]);
                            [world[0], world[1], world[2], a[3], a[4]]
                        })
                        .collect()
                })
                .unwrap_or_default();
            if verts.len() == 4 {
                let n: Vec<f64> = normal
                    .as_array()
                    .map(|c| c.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect())
                    .unwrap_or_default();
                let n = mat_apply(rot, [n[0], n[1], n[2]]);
                out.push(Face {
                    verts: verts.try_into().unwrap(),
                    normal: n,
                });
            }
        }
        if let Some(children) = node["children"].as_object() {
            for child in children.values() {
                visit(child, pivot, rot, out);
            }
        }
    }
    visit(&pack.model, [0.0; 3], IDENTITY, &mut faces);

    // Frame the body cluster: props (bows, shields, background planes)
    // scatter far from the median, so vertices near the median define the
    // character's true bounds.
    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    let mut zs: Vec<f64> = Vec::new();
    for f in &faces {
        for v in &f.verts {
            xs.push(v[0]);
            ys.push(v[1]);
            zs.push(v[2]);
        }
    }
    let median = |mut vals: Vec<f64>| -> f64 {
        vals.sort_by(f64::total_cmp);
        vals.get(vals.len() / 2).copied().unwrap_or(0.0)
    };
    let (mx, mz) = (median(xs.clone()), median(zs.clone()));
    let body: Vec<[f64; 2]> = xs
        .iter()
        .zip(&ys)
        .zip(&zs)
        .filter(|((x, _), z)| (**x - mx).abs() < 20.0 && (**z - mz).abs() < 20.0)
        .map(|((x, y), _)| [*x, *y])
        .collect();
    let (bx0, bx1, by0, by1) = body.iter().fold(
        (f64::MAX, f64::MIN, f64::MAX, f64::MIN),
        |(a, b, c, d), [x, y]| (a.min(*x), b.max(*x), c.min(*y), d.max(*y)),
    );
    let center = [(bx0 + bx1) / 2.0, (by0 + by1) / 2.0, mz];
    let span = ((bx1 - bx0).max(by1 - by0) * 1.15).max(1.0);

    let (tw, th, tex) = &pack.texture;
    let sample = |u: f64, v: f64| -> [u8; 4] {
        let x = (u * f64::from(*tw)).clamp(0.0, f64::from(tw.saturating_sub(1))) as usize;
        let y = (v * f64::from(*th)).clamp(0.0, f64::from(th.saturating_sub(1))) as usize;
        let i = (y * *tw as usize + x) * 4;
        tex[i..i + 4].try_into().unwrap_or([255, 0, 255, 255])
    };
    let scale = f64::from(size) * 0.82 / span;
    let render = |cam: f64| -> (Vec<u8>, f64) {
        let light = {
            let l = [0.25 * cam, -0.5, 0.85 * cam];
            let len = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
            [l[0] / len, l[1] / len, l[2] / len]
        };
        let mut image = vec![0u8; size as usize * size as usize * 4];
        let mut zbuf = vec![f64::NEG_INFINITY; size as usize * size as usize];
        let mut brightness = 0.0;
        let mut lit = 0usize;
        for f in &faces {
            let shade = 0.55
                + 0.45
                    * (f.normal[0] * light[0]
                        + f.normal[1] * light[1]
                        + f.normal[2] * light[2])
                        .max(0.0);
            let vs: [[f64; 5]; 4] = f.verts.map(|v| {
                [
                    (cam * (v[0] - center[0])) * scale + f64::from(size) / 2.0,
                    (v[1] - center[1]) * scale + f64::from(size) / 2.0,
                    cam * v[2],
                    v[3],
                    v[4],
                ]
            });
            for (a, b, c) in [(0usize, 1, 2), (0, 2, 3)] {
                let (p0, p1, p2) = (vs[a], vs[b], vs[c]);
                let area = (p1[0] - p0[0]) * (p2[1] - p0[1]) - (p1[1] - p0[1]) * (p2[0] - p0[0]);
                if area.abs() < 1e-9 {
                    continue;
                }
                let (x_min, x_max) = (
                    p0[0].min(p1[0]).min(p2[0]).floor().max(0.0) as i64,
                    p0[0].max(p1[0]).max(p2[0]).ceil().min(f64::from(size - 1)) as i64,
                );
                let (y_min, y_max) = (
                    p0[1].min(p1[1]).min(p2[1]).floor().max(0.0) as i64,
                    p0[1].max(p1[1]).max(p2[1]).ceil().min(f64::from(size - 1)) as i64,
                );
                for y in y_min..=y_max {
                    for x in x_min..=x_max {
                        let (px, py) = (f64::from(x as i32) + 0.5, f64::from(y as i32) + 0.5);
                        let w0 = ((p1[0] - p0[0]) * (py - p0[1]) - (p1[1] - p0[1]) * (px - p0[0]))
                            / area;
                        let w1 = ((p2[0] - p1[0]) * (py - p1[1]) - (p2[1] - p1[1]) * (px - p1[0]))
                            / area;
                        let w2 = ((p0[0] - p2[0]) * (py - p2[1]) - (p0[1] - p2[1]) * (px - p2[0]))
                            / area;
                        if w0 < -1e-9 || w1 < -1e-9 || w2 < -1e-9 {
                            continue;
                        }
                        let z = w0 * p0[2] + w1 * p1[2] + w2 * p2[2];
                        let idx = y as usize * size as usize + x as usize;
                        if z <= zbuf[idx] {
                            continue;
                        }
                        zbuf[idx] = z;
                        let u = w0 * p0[3] + w1 * p1[3] + w2 * p2[3];
                        let v = w0 * p0[4] + w1 * p1[4] + w2 * p2[4];
                        let [r, g, b, alpha] = sample(u, v);
                        let k = idx * 4;
                        image[k] = (f64::from(r) * shade).min(255.0) as u8;
                        image[k + 1] = (f64::from(g) * shade).min(255.0) as u8;
                        image[k + 2] = (f64::from(b) * shade).min(255.0) as u8;
                        image[k + 3] = alpha;
                        brightness += f64::from(u16::from(r) + u16::from(g) + u16::from(b)) * f64::from(alpha) / 765.0;
                        lit += 1;
                    }
                }
            }
        }
        (image, if lit > 0 { brightness / lit as f64 } else { 0.0 })
    };
    let (front, bright_front) = render(1.0);
    let (back, bright_back) = render(-1.0);
    if bright_back > bright_front {
        (size, size, back)
    } else {
        (size, size, front)
    }
}

/// Encodes RGBA rows as a PNG.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer
            .write_image_data(rgba)
            .map_err(|e| e.to_string())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1x1 RGBA PNG (all transparent), base64: every bbmodel embeds its
    /// textures like this.
    const P1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";



    /// A minimal player-shaped bbmodel: one `Head` bone (pivot 0,24,0) with
    /// the vanilla head's 8x8x8 cube, per-face uvs of a 64x64 skin, and a
    /// `Body` bone. The baked output must match vanilla's own exporter.
    fn head_bbmodel() -> String {
        let face = |u: [f64; 4], tex: u64| {
            format!(r#"{{"uv": [{}, {}, {}, {}], "texture": {}}}"#, u[0], u[1], u[2], u[3], tex)
        };
        let faces = format!(
            r#"{{
                "north": {}, "east": {}, "south": {}, "west": {}, "up": {}, "down": {}
            }}"#,
            face([8.0, 8.0, 16.0, 16.0], 0),
            face([16.0, 8.0, 24.0, 16.0], 0),
            face([24.0, 8.0, 32.0, 16.0], 0),
            face([0.0, 8.0, 8.0, 16.0], 0),
            face([8.0, 0.0, 16.0, 8.0], 0),
            face([16.0, 0.0, 24.0, 8.0], 0),
        );
        format!(
            r#"{{
              "meta": {{ "format_version": "4.5" }},
              "resolution": {{ "width": 64, "height": 64 }},
              "elements": [
                {{
                  "uuid": "cube-head", "type": "cube", "name": "head cube",
                  "from": [-4, 24, -4], "to": [4, 32, 4],
                  "box_uv": false, "faces": {faces}
                }}
              ],
              "outliner": [
                {{
                  "uuid": "bone-head", "name": "Head", "origin": [0, 24, 0],
                  "rotation": [0, 0, 0], "children": ["cube-head"]
                }}
              ],
              "textures": [
                {{ "uuid": "t0", "name": "skin.png", "source": "data:image/png;base64,{P1X1}" }}
              ]
            }}"#
        )
    }

    #[test]
    fn a_player_head_bakes_like_vanilla() {
        let pack = convert(head_bbmodel().as_bytes(), &Value::Null).unwrap();
        let head = &pack.model["children"]["head"];
        assert_eq!(
            head["pose"].as_array().unwrap()[..6]
                .iter()
                .map(|x| x.as_f64().unwrap_or(0.0))
                .collect::<Vec<_>>(),
            vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            "pivot (0, 24, 0) maps to the vanilla head's (0, 0, 0)"
        );
        let polys = head["polygons"].as_array().unwrap();
        assert_eq!(polys.len(), 6, "six faces");
        // The top face: the game's model space has Y down, so the top of the
        // bbmodel cube (y 24..32) is the negative-y face, exactly vanilla's.
        let top = &polys[0];
        let verts: Vec<[f64; 5]> = top
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                let a: Vec<f64> = v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap_or(0.0)).collect();
                [a[0], a[1], a[2], a[3], a[4]]
            })
            .collect();
        let expected: [[f64; 5]; 4] = [
            [4.0, -8.0, 4.0, 0.25, 0.0],
            [-4.0, -8.0, 4.0, 0.125, 0.0],
            [-4.0, -8.0, -4.0, 0.125, 0.125],
            [4.0, -8.0, -4.0, 0.25, 0.125],
        ];
        for (got, want) in verts.iter().zip(expected) {
            for i in 0..5 {
                assert!(
                    (got[i] - want[i]).abs() < 1e-6,
                    "vertex {i}: {got:?} vs {want:?}"
                );
            }
        }
    }

    #[test]
    fn uvs_remap_across_stitched_textures() {
        // Two textures, the cube reading texture 1: its uvs shift down by the
        // first texture's height (1 pixel).
        let bb = head_bbmodel()
            .replace(r#""texture": 0"#, r#""texture": 1"#)
            .replace(
                r#""textures": ["#,
                r#""textures": [
                { "uuid": "t0", "name": "a.png", "source": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==" },"#,
            );
        let pack = convert(bb.as_bytes(), &Value::Null).unwrap();
        assert_eq!(pack.texture.1, 2, "two 1px textures stacked");
        let top = &pack.model["children"]["head"]["polygons"][0];
        let u: f64 = top[0][3].as_f64().unwrap();
        let v: f64 = top[0][4].as_f64().unwrap();
        // The top face read (16, 0) of a 64-canvas from texture 1, which is
        // the second row of the 1x2 atlas: u 16/64 * 1px / 1px wide, shifted
        // v 1/2 (the texture's own offset in the stitched atlas).
        assert!((u - (16.0 / 64.0)).abs() < 1e-6, "u {u}");
        assert!((v - 0.5).abs() < 1e-6, "v {v}");
    }

    #[test]
    fn unmapped_bones_keep_static_poses() {
        let bb = head_bbmodel()
            .replace(r#""name": "Head""#, r#""name": "Backpack""#)
            .replace(r#""origin": [0, 24, 0]"#, r#""origin": [0, 30, 0]"#);
        let pack = convert(bb.as_bytes(), &Value::Null).unwrap();
        let backpack = &pack.model["children"]["backpack_0"];
        assert!(!backpack.is_null(), "unmapped bones keep their own name");
        let py = backpack["pose"][1].as_f64().unwrap();
        assert!((py - (24.0 - 30.0)).abs() < 1e-9, "pivot y maps down: {py}");
    }
}
