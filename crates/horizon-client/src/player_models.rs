//! Custom player models: the launcher-side toggle plus `.bbmodel` / `.ysm`
//! packs imported into `player-models/` in the game folder (the folder the
//! launcher writes `settings.json` into, beside the model files the player
//! imports there). Everything here reads at world load; nothing runs per
//! frame.
//!
//! The folder layout:
//!
//! ```text
//! player-models/
//!   settings.json        { "enabled": bool, "model": "name.ysm", "texture": 0 }
//!   name.bbmodel         a Blockbench model, or
//!   name.ysm             a Yes Steve Model package
//!   name.json            optional bbmodel mapping overrides (flip_x, mc_height)
//!   cache/               converted model + texture, keyed by stem and texture
//!   previews/            rendered thumbnails for the launcher's model page
//! ```
//!
//! When enabled and the model file loads, the game's own player model, skin
//! and cape layers are replaced: the pack's part tree is drawn with the
//! game's humanoid animations driving the bones whose names match the
//! vanilla skeleton, and the pack's stitched texture shows through the
//! `horizon/cpm` atlas sprite (horizon-client-render).
use crate::mobs::Part;
use horizon_assets::player_model;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The atlas sprite the pack's texture is uploaded to (`horizon/cpm`,
/// uploaded by horizon-client-render's atlas builder when the pack loads).
pub use horizon_client_render::CPM_SPRITE as TEXTURE_SPRITE;

/// The folder in the game folder.
pub const FOLDER: &str = "player-models";

/// The model extensions the folder accepts: YSM packages, Blockbench models,
/// and a plain `.zip` holding either (what people actually download).
const EXTENSIONS: [&str; 3] = ["ysm", "bbmodel", "zip"];

struct Settings {
    enabled: bool,
    model: String,
    texture: usize,
}

fn settings(root: &Path) -> Option<Settings> {
    let bytes = std::fs::read(root.join(FOLDER).join("settings.json")).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    Some(Settings {
        enabled: v["enabled"].as_bool()?,
        model: v["model"].as_str()?.to_owned(),
        texture: v["texture"].as_u64().unwrap_or(0) as usize,
    })
}

/// A model pack installed in the folder, as the wheel and the launcher list.
pub struct InstalledModel {
    /// The display name (the pack's own metadata, else the file stem).
    pub name: String,
    /// The file name inside `player-models/` (the settings' `model` value).
    pub file: String,
    /// How many textures (materials) the pack offers to switch between.
    /// A bbmodel offers its embedded textures; a ysm its `files.player.texture`
    /// list.
    pub textures: usize,
}

/// Every model pack in the folder, sorted by display name.
pub fn list_models(root: &Path) -> Vec<InstalledModel> {
    let Ok(entries) = std::fs::read_dir(root.join(FOLDER)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !EXTENSIONS.contains(&ext) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let textures = match ext {
            // Both a `.ysm` package and a zip holding one carry `ysm.json`
            // with the material list; a zip with a bare bbmodel falls back to
            // that model's embedded textures.
            "ysm" | "zip" => zip_texture_count(&bytes)
                .or_else(|| bbmodel_texture_count(&bytes))
                .unwrap_or(1),
            // A bbmodel's textures: each embedded image is one material.
            _ => bbmodel_texture_count(&bytes).unwrap_or(1),
        };
        let name = player_model::peek_model_name(&bytes).unwrap_or_else(|| stem.clone());
        out.push(InstalledModel {
            name,
            file: entry.file_name().to_string_lossy().into_owned(),
            textures,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Writes the selection: which model is active, its texture, and whether the
/// whole feature is on.
pub fn write_settings(root: &Path, enabled: bool, model: &str, texture: usize) {
    let folder = root.join(FOLDER);
    let _ = std::fs::create_dir_all(&folder);
    let _ = std::fs::write(
        folder.join("settings.json"),
        json!({ "enabled": enabled, "model": model, "texture": texture }).to_string(),
    );
}

/// The model file's name from the settings, if the folder holds it.
pub fn selected_model(root: &Path) -> Option<PathBuf> {
    let s = settings(root)?;
    if !s.enabled || s.model.is_empty() {
        return None;
    }
    let file = root.join(FOLDER).join(&s.model);
    file.is_file().then_some(file)
}

/// The current selection as `(file, texture_index)` when the feature is on.
pub fn selected_info(root: &Path) -> Option<(String, usize)> {
    let s = settings(root)?;
    s.enabled.then_some((s.model, s.texture))
}

/// The selection as stored, whether or not the pack is in use right now.
fn stored_selection(root: &Path) -> (String, usize) {
    std::fs::read(root.join(FOLDER).join("settings.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .map(|v| {
            (
                v["model"].as_str().unwrap_or_default().to_owned(),
                v["texture"].as_u64().unwrap_or(0) as usize,
            )
        })
        .unwrap_or_default()
}

/// Whether the pack replaces the player right now (`enabled` in settings).
pub fn enabled(root: &Path) -> bool {
    settings(root).is_some_and(|s| s.enabled)
}

/// 「取消变身 / 恢复变身」: writes the flag and drops the live pack so the
/// vanilla Steve model stands in at once instead of at the next world load.
/// The model and material stay remembered, so turning it back on restores
/// exactly what was worn.
pub fn set_transform(root: &Path, on: bool) {
    let (model, texture) = stored_selection(root);
    write_settings(root, on, &model, texture);
    if on {
        refresh_loaded(root);
        return;
    }
    *CUSTOM_MODEL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *CUSTOM_ANIMS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *CUSTOM_KEY.lock().unwrap_or_else(|e| e.into_inner()) = None;
    clear_action();
}

/// 内置酒狐模型（来自 TartaricAcid/WineFoxModel，随包分发）。
const BUILTIN_WINEFOX: &[u8] =
    include_bytes!("../../horizon-assets/tests/fixtures/winefox.bbmodel");

/// 内置模型的文件名（settings.json 的 `model` 值）。
pub const BUILTIN_NAME: &str = "酒狐.bbmodel";

/// 确保内置酒狐模型存在于 player-models 目录（已存在则跳过；这是分支里
/// 唯一的随包模型，导入入口已经移除，想换模型直接替换这个文件即可）。
pub fn seed_builtin(root: &Path) {
    let dir = root.join(FOLDER);
    let _ = std::fs::create_dir_all(&dir);
    let target = dir.join(BUILTIN_NAME);
    if !target.exists() {
        let _ = std::fs::write(&target, BUILTIN_WINEFOX);
    }
    // 首次运行默认变身为酒狐；之后完全交给「变身 / 取消变身」按钮，
    // 启动时不再强制改写，否则取消变身会在下次开游戏时被翻回来。
    if !dir.join("settings.json").exists() {
        write_settings(root, true, BUILTIN_NAME, 0);
    }
}

/// Whether the converted cache (model JSON + texture PNG) is stale: missing,
/// or older than the source model.
fn cache_fresh(cache: &Path, source: &Path) -> bool {
    let model = cache.join("model.json");
    let texture = cache.join("texture.png");
    let (Ok(model), Ok(texture)) = (std::fs::metadata(&model), std::fs::metadata(&texture))
    else {
        return false;
    };
    let Ok(source) = std::fs::metadata(source) else {
        return false;
    };
    let newest_cache = model
        .modified()
        .ok()
        .zip(texture.modified().ok())
        .map(|(a, b)| a.max(b));
    match (newest_cache, source.modified().ok()) {
        (Some(cache), Some(source)) => cache >= source,
        _ => false,
    }
}

/// Reads a cached conversion (model JSON bytes, texture PNG bytes,
/// animations JSON bytes).
fn read_cache(cache: &Path) -> Option<(Vec<u8>, Vec<u8>, Value)> {
    let model = std::fs::read(cache.join("model.json")).ok()?;
    let texture = std::fs::read(cache.join("texture.png")).ok()?;
    let animations = std::fs::read(cache.join("animations.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    Some((model, texture, animations))
}

fn write_cache(cache: &Path, model: &Value, texture: &(u32, u32, Vec<u8>), animations: &Value) {
    let _ = std::fs::create_dir_all(cache);
    let _ = std::fs::write(cache.join("model.json"), model.to_string());
    let _ = std::fs::write(cache.join("animations.json"), animations.to_string());
    if let Ok(png) = player_model::encode_png(texture.0, texture.1, &texture.2) {
        let _ = std::fs::write(cache.join("texture.png"), png);
    }
}

/// Decodes the cached texture PNG back into RGBA rows (the atlas builder and
/// the bake path want pixels, the cache stores an encoded PNG).
fn cached_texture(png: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    player_model::decode_png(png).ok()
}

/// The cache folder one pack + material bakes into. The "v10" prefix retires
/// caches baked before Blockbench's `visibility` flag was honoured (spare
/// shapes — a fox form, alt outfits — drew extra limbs over the player) and
/// before the wrong bone rest-rotation signs were fixed (a lean about x or z
/// came out reversed: the fox tail folded down through the legs, arms posed
/// by a z lean turned into the torso).
fn cache_dir(root: &Path, file: &Path, texture: usize) -> PathBuf {
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    root.join(FOLDER)
        .join("cache")
        .join(format!("v10-model-{stem}-{texture}"))
}

/// Reads one pack's baked output — model JSON, texture pixels, animations —
/// from the cache, converting (and caching) on a miss. Never touches the
/// live player state, so the wheel can ask about a pack that is not the one
/// being worn.
fn load_pack(
    root: &Path,
    file: &Path,
    texture: usize,
) -> Option<(Value, (u32, u32, Vec<u8>), Animations)> {
    let cache = cache_dir(root, file, texture);
    if cache_fresh(&cache, file)
        && let Some((model, png, animations)) = read_cache(&cache)
        && let Ok(model) = serde_json::from_slice::<Value>(&model)
        && let Some(texture) = cached_texture(&png)
    {
        return Some((model, texture, parse_animations(&animations)));
    }
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let bytes = std::fs::read(file).ok()?;
    let pack = convert_file(file, &bytes, texture)
        .map_err(|e| {
            eprintln!("[player-models] {stem}: {e}");
            e
        })
        .ok()?;
    write_cache(&cache, &pack.model, &pack.texture, &pack.animations);
    Some((
        pack.model,
        pack.texture,
        parse_animations(&pack.animations),
    ))
}

/// The pack's baked part tree when the toggle is on and the model converts:
/// `(root part, texture sprite name, texture size and pixels)`. Errors
/// surface as `None` (the vanilla player model stands in), with the reason
/// in the game log.
pub fn custom_player_model(
    root: &Path,
) -> Option<(Part, &'static str, (u32, u32, Vec<u8>), Animations)> {
    let file = selected_model(root)?;
    let s = settings(root).unwrap_or(Settings {
        enabled: true,
        model: String::new(),
        texture: 0,
    });
    let (model, texture, anims) = load_pack(root, &file, s.texture)?;
    remember_animations(&anims);
    remember_loaded(&file, s.texture);
    let part = Part::from_value("root", &model);
    remember_model(&part);
    Some((part, TEXTURE_SPRITE, texture, anims))
}

/// The custom model's keyframe animations (the game's schema), keyed by name.
pub type Animations = std::collections::HashMap<String, crate::mobs::keyframes::Animation>;

/// Parses the baked animation set from its JSON (null or empty → empty map).
fn parse_animations(v: &Value) -> Animations {
    v.as_object()
        .map(|all| {
            all.iter()
                .map(|(name, a)| (name.clone(), crate::mobs::keyframes::Animation::parse(a)))
                .collect()
        })
        .unwrap_or_default()
}

/// The loaded custom model's animations (None while the vanilla player is up).
/// Filled by [`custom_player_model`]; the humanoid animation reads it to play
/// the pack's own idle / walk / sneak / swim... keyframe animations.
static CUSTOM_ANIMS: Mutex<Option<Arc<Animations>>> = Mutex::new(None);

/// Which pack (file name, material) those animations came from: the wheel
/// uses it to tell "this is already the worn pack" from "read that one".
static CUSTOM_KEY: Mutex<Option<(String, usize)>> = Mutex::new(None);

fn remember_loaded(file: &Path, texture: usize) {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    *CUSTOM_KEY.lock().unwrap_or_else(|e| e.into_inner()) = Some((name, texture));
}

fn loaded_is(file: &str, texture: usize) -> bool {
    CUSTOM_KEY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|(name, t)| name == file && *t == texture)
}

/// The loaded pack's part tree, kept for the first-person arm: the hand is
/// the pack's own `right_arm` subtree, not the vanilla skin's.
static CUSTOM_MODEL: Mutex<Option<Arc<Part>>> = Mutex::new(None);

/// Whether a custom pack is driving the player right now: the humanoid setup
/// steps aside for it (the pack's skeleton already carries its rest pose).
pub fn custom_model_active() -> bool {
    CUSTOM_MODEL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some()
}

/// The pack's part tree when a custom model is up (the first-person hand).
/// The world load fills it through [`custom_player_model`]; should the first
/// person frame come before that (or a cached type skip the load), one retry
/// from the process folder bakes it here.
pub fn custom_hand_model() -> Option<Arc<Part>> {
    if let Some(model) = CUSTOM_MODEL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return Some(model);
    }
    static TRIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !TRIED.swap(true, std::sync::atomic::Ordering::Relaxed)
        && let Ok(cwd) = std::env::current_dir()
    {
        let _ = custom_player_model(&cwd);
    }
    CUSTOM_MODEL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Publishes the conversion's part tree (idempotent; the last load wins).
fn remember_model(part: &Part) {
    *CUSTOM_MODEL.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(part.clone()));
}

/// The action the model wheel picked: `(animation name, started at)` for the
/// local player. Actions play in real time; a non-looping one clears itself
/// once done.
static PLAYER_ACTION: Mutex<Option<(String, std::time::Instant)>> = Mutex::new(None);

/// Publishes a conversion's animations (idempotent; the last load wins).
fn remember_animations(anims: &Animations) {
    *CUSTOM_ANIMS.lock().unwrap_or_else(|e| e.into_inner()) = if anims.is_empty() {
        None
    } else {
        Some(Arc::new(anims.clone()))
    };
}

/// The names a pack offers on the action wheel: every animation that is not
/// an auto state (idle / walk / run / sneak / swim / ride / sleep / death /
/// hurt / elytra / the ambient `parallel` layers). YSM's own roulette lists
/// the pack's animations by name and only hides those states, so a model
/// whose actions are named in Chinese or `play1`, `sit`... still shows them.
fn action_names(anims: &Animations) -> Vec<String> {
    let is_state = |name: &str| {
        let n = name.to_ascii_lowercase();
        n.starts_with("idle")
            || n.starts_with("walk")
            || n.starts_with("run")
            || n.starts_with("sneak")
            || n.starts_with("swim")
            || n.starts_with("ride")
            || n.starts_with("elytra")
            || n.starts_with("jump")
            || n.contains("parallel")
            || matches!(n.as_str(), "sleep" | "death" | "attacked")
    };
    let mut names: Vec<String> = anims
        .keys()
        .filter(|n| !is_state(n))
        .cloned()
        .collect();
    names.sort();
    names
}

/// One entry on the action wheel: the animation to play and the label to
/// show. `label` is empty when the pack declares none — the wheel then
/// translates the raw name (dance1 → 舞蹈 1).
pub struct WheelAction {
    pub key: String,
    pub label: String,
}

/// The action names the wheel offers for the pack that is playing now.
pub fn action_list() -> Vec<String> {
    let anims = CUSTOM_ANIMS.lock().unwrap_or_else(|e| e.into_inner());
    anims.as_ref().map_or_else(Vec::new, |a| action_names(a))
}

/// The actions a specific pack offers. A YSM package declares its own
/// roulette menu in `ysm.json` (`properties.extra_animation`, plus each
/// `extra_animation_classify` group) — that is what the mod shows, so it
/// wins over the raw animation list. Everything else (a bare bbmodel) falls
/// back to the pack's non-state animation names. Reading the requested pack
/// (cache first) instead of the worn one is what makes switching models
/// update the wheel.
pub fn actions_for(root: &Path, file: &str, texture: usize) -> Vec<WheelAction> {
    let path = root.join(FOLDER).join(file);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
    if matches!(ext, "ysm" | "zip")
        && let Ok(bytes) = std::fs::read(&path)
    {
        let declared = player_model::ysm_actions(&bytes);
        if !declared.is_empty() {
            return declared
                .into_iter()
                .map(|(key, label)| WheelAction { key, label })
                .collect();
        }
    }
    let loaded = if loaded_is(file, texture) {
        CUSTOM_ANIMS.lock().unwrap_or_else(|e| e.into_inner()).clone()
    } else {
        None
    };
    let names = match loaded {
        Some(anims) => action_names(&anims),
        None => load_pack(root, &path, texture)
            .map(|(_, _, anims)| action_names(&anims))
            .unwrap_or_default(),
    };
    names
        .into_iter()
        .map(|key| WheelAction {
            key,
            label: String::new(),
        })
        .collect()
}

/// Re-reads the selected pack's animations (and part tree) so the wheel and
/// the state machine follow a freshly applied model right away instead of
/// keeping the previous pack's set until the player entity is rebuilt.
pub fn refresh_loaded(root: &Path) {
    let _ = custom_player_model(root);
}

/// The wheel's pick: play `name` on the local player right away.
pub fn trigger_action(name: &str) {
    *PLAYER_ACTION.lock().unwrap_or_else(|e| e.into_inner()) =
        Some((name.to_owned(), std::time::Instant::now()));
}

/// The wheel's "stop" entry.
pub fn clear_action() {
    *PLAYER_ACTION.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Samples one animation channel like `KeyframeAnimation.Entry.apply` does.
fn sample_channel(k: &[crate::mobs::keyframes::Keyframe], seconds: f32, scale: f32) -> [f32; 3] {
    use crate::mobs::keyframes::Keyframe;
    if k.is_empty() {
        return [0.0; 3];
    }
    let first = k.iter().position(|f: &Keyframe| seconds <= f.time).unwrap_or(k.len());
    let prev = first.saturating_sub(1);
    let next = (prev + 1).min(k.len() - 1);
    let delta = seconds - k[prev].time;
    let alpha = if next != prev {
        (delta / (k[next].time - k[prev].time)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    if k[next].catmull {
        let p0 = k[prev.saturating_sub(1)].post;
        let p1 = k[prev].post;
        let p2 = k[next].post;
        let p3 = k[(next + 1).min(k.len() - 1)].post;
        let t = alpha;
        let catmullrom = |a: f32, b: f32, c: f32, d: f32| {
            0.5 * (2.0 * b
                + (c - a) * t
                + (2.0 * a - 5.0 * b + 4.0 * c - d) * t * t
                + (3.0 * b - a - 3.0 * c + d) * t * t * t)
        };
        std::array::from_fn(|i| catmullrom(p0[i], p1[i], p2[i], p3[i]) * scale)
    } else {
        let a = k[prev].post;
        let b = k[next].pre;
        std::array::from_fn(|i| (a[i] + (b[i] - a[i]) * alpha) * scale)
    }
}

/// Applies one animation to the pose map at `millis`.
fn apply_animation(
    anim: &crate::mobs::keyframes::Animation,
    millis: i64,
    scale: f32,
    poses: &mut crate::mobs::anim::Poses,
) {
    let mut seconds = millis as f32 / 1000.0;
    if anim.looping {
        seconds = if anim.length > 0.0 {
            seconds % anim.length
        } else {
            0.0
        };
    } else if anim.length > 0.0 {
        seconds = seconds.min(anim.length);
    }
    for (bone, channels) in &anim.bones {
        for channel in channels {
            let p = poses.get(bone);
            use crate::mobs::keyframes::Target;
            // 幅度缩放只作用于旋转与位移；缩放通道必须按原值走，否则 walk
            // 里 `Root scale = 1` 会被乘成 0.3，模型一走就整只缩小。
            let amplitude = if channel.target == Target::Scale { 1.0 } else { scale };
            let v = sample_channel(&channel.keyframes, seconds, amplitude);
            match channel.target {
                Target::Position => {
                    p[0] += v[0];
                    p[1] += v[1];
                    p[2] += v[2];
                }
                Target::Rotation => {
                    // YSM offsets from the bind pose, it does not replace it:
                    // `MathUtil.lerpRotationValues` evaluates
                    // `getQuatFromEulerZYX(anim + initRot)` and stores
                    // `euler - initRot` back. An idle keyframe of `0,0,0`
                    // therefore means "keep the rest angle", not "straighten
                    // the bone" — replacing turned the fox's arms inward by
                    // their -18° lean and the hands read as swallowed by the
                    // torso.
                    p[3] += v[0];
                    p[4] += v[1];
                    p[5] += v[2];
                }
                Target::Scale => {
                    // A zero scale is YSM's way of hiding a spare part (the
                    // mask: pre_parallel1 sets it to 0). Keep it a hair above
                    // zero so the normal matrix never inverts a singular one.
                    (p[6], p[7], p[8]) = (
                        v[0].max(0.0001),
                        v[1].max(0.0001),
                        v[2].max(0.0001),
                    );
                }
            }
        }
    }
}

/// The custom model's own animation runtime, run after the humanoid setup:
/// the wheel's action wins, else the state picks idle / walk / run / sneak /
/// swim / ride / sleep / death..., and the parallel layers always add their
/// ambient motion (tail sway, cloth, hair). The vanilla humanoid rotations
/// survive on any bone the pack's animations don't drive (head look).
pub fn apply_player_animations(
    m: &crate::mobs::Mob,
    s: &crate::mobs::anim::State,
    poses: &mut crate::mobs::anim::Poses,
) {
    let anims = CUSTOM_ANIMS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(anims) = anims.as_ref() else {
        return;
    };
    let mut state_played = false;
    // The wheel's action (the local player only, mob id 0): a looping one
    // holds until replaced, a once-animation clears itself when done.
    //
    // Moving, jumping/falling and taking a hit cut it short, the way a pose
    // mod gives the body back to the walk cycle: a dance that kept playing
    // while the player ran looked like the legs had stopped responding.
    let interrupted = s.speed > 0.12 || m.hurt_time > 0 || !m.on_ground;
    if m.id == 0 {
        let mut slot = PLAYER_ACTION.lock().unwrap_or_else(|e| e.into_inner());
        if interrupted {
            *slot = None;
        }
        if let Some((name, at)) = slot.as_ref() {
            match anims.get(name) {
                Some(anim) => {
                    let elapsed = at.elapsed().as_millis() as i64;
                    let done = !anim.looping && anim.length > 0.0 && elapsed as f32 > anim.length * 1000.0;
                    if done {
                        *slot = None;
                    } else {
                        apply_animation(anim, elapsed, 1.0, poses);
                        state_played = true;
                    }
                }
                None => *slot = None,
            }
        }
    }
    if !state_played {
        let pick = state_animation(m, s, &anims);
        if let Some((name, millis, scale)) = pick
            && let Some(anim) = anims.get(name.as_str())
        {
            apply_animation(anim, millis, scale, poses);
        }
    }
    // The parallel layers ride on top of any state (ambient tail, cloth...).
    // YSM names them `pre_parallel0` / `pre_parallel2`: they carry the rest
    // angles of hair and tail and the default visibility of spares (the mask
    // is scaled to 0), so a plain `starts_with("parallel")` misses all of them.
    for (name, anim) in anims.iter() {
        if name.contains("parallel") {
            apply_animation(anim, (s.age * 50.0) as i64, 1.0, poses);
        }
    }
}

/// The pack's clip for a hand state: the exact name, else any
/// `name:<item>` variant (YSM ships item-conditional clips like
/// `use_mainhand:gohei`).
fn hand_clip<'a>(
    anims: &'a Animations,
    base: &str,
) -> Option<(&'a str, &'a crate::mobs::keyframes::Animation)> {
    if let Some((name, anim)) = anims.get_key_value(base) {
        return Some((name.as_str(), anim));
    }
    anims
        .iter()
        .find(|(name, _)| name.strip_prefix(base).is_some_and(|rest| rest.starts_with(':')))
        .map(|(name, anim)| (name.as_str(), anim))
}

/// Whether the pack itself animates the hands for this state, in which case
/// the vanilla arm pose must not be layered on top. A pack without the clip
/// (most bbmodels) still needs the vanilla arms, or a used item or a swing
/// leaves the hands hanging.
pub fn pack_drives_hands(m: &crate::mobs::Mob, s: &crate::mobs::anim::State) -> bool {
    let anims = CUSTOM_ANIMS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(anims) = anims.as_ref() else {
        return false;
    };
    if m.extra.using_item > 0 {
        let base = if m.extra.using_off_hand {
            "use_offhand"
        } else {
            "use_mainhand"
        };
        return hand_clip(anims, base).is_some();
    }
    s.swing > 0.0 && hand_clip(anims, "swing_hand").is_some()
}

/// The auto state: `(animation name, clock millis, amplitude scale)`.
fn state_animation(
    m: &crate::mobs::Mob,
    s: &crate::mobs::anim::State,
    anims: &Animations,
) -> Option<(String, i64, f32)> {
    use crate::pose::Pose;
    let has = |name: &str| anims.contains_key(name);
    let age_ms = (s.age * 50.0) as i64;
    let pick = |name: &'static str| -> Option<(String, i64, f32)> {
        has(name).then(|| (name.to_owned(), age_ms, 1.0))
    };
    // A used item and a swing are hand states: YSM plays the pack's own
    // `use_mainhand` / `use_offhand` / `swing_hand` (or an item-conditional
    // variant) so the arm rises exactly the way the author posed it. These
    // outrank walk and idle — the arm has to lift while running, too.
    if m.extra.using_item > 0 {
        let base = if m.extra.using_off_hand {
            "use_offhand"
        } else {
            "use_mainhand"
        };
        if let Some((name, anim)) = hand_clip(anims, base) {
            let played = (m.extra.using_item as f32 + s.partial) * 50.0;
            let millis = if anim.looping {
                played as i64
            } else {
                (played as i64).min((anim.length * 1000.0) as i64)
            };
            return Some((name.to_owned(), millis, 1.0));
        }
    }
    if s.swing > 0.0
        && let Some((name, anim)) = hand_clip(anims, "swing_hand")
    {
        // `swing` is the attack timer counting *down* from 1, so the clip has
        // to run forwards: 0 at the start of the swing, its length at the end.
        let played = (1.0 - s.swing).clamp(0.0, 1.0) * anim.length * 1000.0;
        return Some((name.to_owned(), played as i64, 1.0));
    }
    if m.death_time > 0 {
        return pick("death").map(|(n, _, _)| (n, ((m.death_time as f32) * 50.0) as i64, 1.0));
    }
    if m.extra.avatar_pose == Pose::Sleeping {
        return pick("sleep").map(|(n, _, _)| (n, 0, 1.0));
    }
    if m.extra.passenger {
        return pick("ride").map(|(n, _, _)| (n, 0, 1.0));
    }
    if m.extra.fall_flying {
        return pick("elytra_fly").map(|(n, _, _)| (n, age_ms, 1.0));
    }
    if m.extra.swim.in_water {
        if m.extra.swim.amount(s.partial) > 0.0 {
            return pick("swim").map(|(n, _, _)| (n, age_ms, 1.0));
        }
        return pick("swim_stand").map(|(n, _, _)| (n, age_ms, 1.0));
    }
    if m.extra.avatar_pose == Pose::Crouching || m.extra.crouching {
        return pick("sneak").map(|(n, _, _)| (n, age_ms, 1.0));
    }
    // Attacked flinches over the hurt ticks (once, driven by the hurt clock).
    if m.hurt_time > 0
        && let Some(anim) = anims.get("attacked")
    {
        let millis = ((anim.length - m.hurt_time as f32 / 20.0).max(0.0) * 1000.0) as i64;
        return Some(("attacked".to_owned(), millis, 1.0));
    }
    // The walk cycle clock follows the limb accumulator (applyWalk).
    if s.speed > 0.01 {
        let millis = (s.pos * 50.0) as i64;
        if m.extra.sprinting && s.speed > 0.35 {
            return pick("run").map(|(n, _, _)| (n, millis, 1.0));
        }
        return pick("walk").map(|(n, _, _)| (n, millis, 1.0));
    }
    pick("idle")
}

/// Converts a model file by extension: `.ysm` packages and `.bbmodel` files.
fn convert_file(
    file: &Path,
    bytes: &[u8],
    texture: usize,
) -> Result<player_model::ConvertedPack, String> {
    match file.extension().and_then(|e| e.to_str()) {
        Some("ysm") => player_model::convert_ysm(bytes, Some(texture)),
        Some("zip") => convert_zip(bytes, texture),
        // An optional <name>.json beside the bbmodel calibrates the mapping.
        _ => {
            let manifest = std::fs::read_to_string(file.with_extension("json"))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .unwrap_or(Value::Null);
            player_model::convert(bytes, &manifest)
        }
    }
}

/// A `.zip` is either a YSM package (it holds `ysm.json`) or an archive with
/// a Blockbench model inside — both get imported as one model file.
fn convert_zip(
    bytes: &[u8],
    texture: usize,
) -> Result<player_model::ConvertedPack, String> {
    use std::io::Read as _;
    let ysm = player_model::convert_ysm(bytes, Some(texture));
    if ysm.is_ok() {
        return ysm;
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("zip: {e}"))?;
    let mut found = None;
    for index in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(index) else {
            continue;
        };
        if !entry
            .name()
            .is_ok_and(|n| n.to_ascii_lowercase().ends_with(".bbmodel"))
        {
            continue;
        }
        let mut raw = Vec::new();
        if entry.read_to_end(&mut raw).is_ok() {
            found = Some(raw);
            break;
        }
    }
    let raw = found.ok_or_else(|| "zip: no ysm.json or .bbmodel inside".to_owned())?;
    player_model::convert(&raw, &Value::Null).map_err(|e| format!("bbmodel in zip: {e}"))
}

/// Renders the pack's preview thumbnail into `previews/<stem>.png` (the
/// launcher's model page shows it). Cheap after a conversion: the bake path
/// caches, so the preview re-renders only when the model changed.
pub fn render_preview_png(root: &Path, file: &str) -> Option<Vec<u8>> {
    let full = root.join(FOLDER).join(file);
    let bytes = std::fs::read(&full).ok()?;
    let stem = Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())?;
    let s = settings(root).unwrap_or(Settings {
        enabled: true,
        model: String::new(),
        texture: 0,
    });
    let pack = convert_file(&full, &bytes, s.texture).ok()?;
    let (w, h, rgba) = player_model::render_preview(&pack, 192);
    let png = player_model::encode_png(w, h, &rgba).ok()?;
    // Cache it on disk: the HUD atlas reads `previews/<stem>.png` for the
    // same thumbnails.
    let dir = root.join(FOLDER).join("previews");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{stem}.png")), &png);
    Some(png)
}

/// Counts the textures of a zip: a YSM package's material list, else the
/// media list of a bare YSM `ysm.json`-less archive (none).
fn zip_texture_count(bytes: &[u8]) -> Option<usize> {
    ysm_texture_count(bytes)
}

/// Counts a ysm package's textures (the material list).
fn ysm_texture_count(bytes: &[u8]) -> Option<usize> {
    use std::io::Read as _;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let mut raw = Vec::new();
    archive
        .by_name("ysm.json")
        .ok()?
        .read_to_end(&mut raw)
        .ok()?;
    let v: Value = serde_json::from_slice(&raw).ok()?;
    Some(v["files"]["player"]["texture"].as_array()?.len().max(1))
}

/// Counts a bbmodel's embedded textures (each one is a material).
fn bbmodel_texture_count(bytes: &[u8]) -> Option<usize> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    Some(v["textures"].as_array()?.len().max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fox_pack() -> horizon_assets::player_model::ConvertedPack {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../horizon-assets/tests/fixtures/winefox.bbmodel"
        );
        let bytes = std::fs::read(path).unwrap();
        horizon_assets::player_model::convert(&bytes, &Value::Null).unwrap()
    }

    /// YSM's animation rotations are offsets from the bind pose
    /// (`MathUtil.lerpRotationValues` evaluates `anim + initRot`), so an idle
    /// keyframe of `0,0,0` has to leave the bone's rest lean alone. Treating
    /// it as an absolute rotation straightened the fox's arms, whose rest
    /// leans -18° outward, and pulled the hands into the torso.
    #[test]
    fn idle_rotations_offset_the_rest_pose() {
        let pack = fox_pack();
        let anims = parse_animations(&pack.animations);
        let idle = anims.get("idle").expect("the fox ships an idle clip");
        let model = Part::from_value("root", &pack.model);
        let mut poses = crate::mobs::anim::Poses::new(&model);
        let rest = *poses.get("left_arm");
        assert!(
            (rest[5] + 18f32.to_radians()).abs() < 1e-3,
            "left_arm keeps its -18° rest lean: {rest:?}"
        );
        apply_animation(idle, 0, 1.0, &mut poses);
        let posed = *poses.get("left_arm");
        assert!(
            (posed[5] - rest[5]).abs() < 1e-3,
            "an idle keyframe of 0 must not move the arm: rest {rest:?}, posed {posed:?}"
        );
        assert!(
            (posed[3] - rest[3]).abs() < 1e-3 && (posed[4] - rest[4]).abs() < 1e-3,
            "x / y rest angles survive idle too: {posed:?}"
        );
        // The lean is what keeps the upper arm clear of the torso: the fox's
        // body is 5 wide and the arm starts at x = 2.42, so a straightened
        // arm is hidden by the body and only the cuff shows.
        let rest_box = arm_box(&model, &crate::mobs::anim::Poses::new(&model));
        let mut idled = crate::mobs::anim::Poses::new(&model);
        apply_animation(idle, 0, 1.0, &mut idled);
        let idle_box = arm_box(&model, &idled);
        println!("rest arm x = {rest_box:?}, idle arm x = {idle_box:?}");
        // The torso is 5 wide, so an arm reaching past x = 6 is clear of it;
        // straightening the -18° lean pulled the tip in to 5.6 and the sleeve
        // disappeared behind the body.
        assert!(
            idle_box[1] > 6.2,
            "idle must keep the arm outside the torso: {idle_box:?}"
        );
    }

    fn arm_box(model: &Part, poses: &crate::mobs::anim::Poses) -> [f32; 2] {
        fn rot(p: [f32; 3]) -> [[f32; 3]; 3] {
            let (sx, cx) = p[0].sin_cos();
            let (sy, cy) = p[1].sin_cos();
            let (sz, cz) = p[2].sin_cos();
            let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
            let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
            let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
            let mul = |a: [[f32; 3]; 3], b: [[f32; 3]; 3]| {
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
        fn walk(p: &Part, t: [f32; 3], r: [[f32; 3]; 3], poses: &crate::mobs::anim::Poses, lo: &mut [f32; 2]) {
            let pose = poses.pose(p);
            let d = [pose[0], pose[1], pose[2]];
            let nt = [0, 1, 2].map(|i| t[i] + r[i][0] * d[0] + r[i][1] * d[1] + r[i][2] * d[2]);
            let m = rot([pose[3], pose[4], pose[5]]);
            let mut nr = [[0.0; 3]; 3];
            for i in 0..3 {
                for j in 0..3 {
                    nr[i][j] = (0..3).map(|k| r[i][k] * m[k][j]).sum();
                }
            }
            if p.name == "left_arm" {
                lo[0] = f32::MAX;
                lo[1] = f32::MIN;
            }
            for poly in &p.polygons {
                for v in &poly.vertices {
                    let w = [0, 1, 2].map(|i| nt[i] + nr[i][0] * v[0] + nr[i][1] * v[1] + nr[i][2] * v[2]);
                    if p.name == "left_arm" || p.name == "left_forearm" {
                        lo[0] = lo[0].min(w[0]);
                        lo[1] = lo[1].max(w[0]);
                    }
                }
            }
            for c in &p.children {
                walk(c, nt, nr, poses, lo);
            }
        }
        let mut lo = [f32::MAX, f32::MIN];
        walk(model, [0.0; 3], [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], poses, &mut lo);
        lo
    }
}
