//! The custom player model wheel (`key.playerModel`, default Y) and the YSM
//! style action wheel (`key.playerActions`, default F4): tap to open, tap a
//! slot to apply. Two tabs — 模型 picks the model pack in `player-models/`,
//! 动作 plays one of the pack's own keyframe animations (dances, emotes).
//! Textures switch on the strip below the wheel and apply when the atlas
//! rebuilds (next world join or resource reload).
use super::{Game, Screen};
use horizon_client::{hud::HudBatch, keybinds, text::Text};

/// The slot diameter and the wheel radius, in GUI pixels.
const SLOT: i32 = 30;
const RADIUS: i32 = 66;
/// The texture strip's slot size.
const TEX_SLOT: i32 = 18;

/// The wheel's two tabs: the model picker and the action picker.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Models,
    Actions,
}

/// A clickable rect from the last draw: an action row, the stop entry or a
/// tab switch.
#[derive(Clone, Copy)]
struct WheelButton {
    kind: WheelButtonKind,
    rect: [i32; 4],
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum WheelButtonKind {
    Action(usize),
    Stop,
    /// 恢复原版史蒂夫模型（取消变身）。
    Vanilla,
    TabModels,
    TabActions,
    PagePrev,
    PageNext,
}

pub(super) struct PlayerModelScreen {
    /// The models in `player-models/`, in display order.
    models: Vec<horizon_client::player_models::InstalledModel>,
    /// Which tab is showing.
    tab: Tab,
    /// The pack's playable actions, in display order.
    actions: Vec<horizon_client::player_models::WheelAction>,
    /// The clickable rects of the last draw (actions tab).
    buttons: Vec<WheelButton>,
    /// The action ring's page (eight slots a page, like YSM's roulette).
    action_page: usize,
    /// The wheel slot under the cursor, if any.
    hovered: Option<usize>,
    /// The selected model's index (applied on close).
    selected: usize,
    /// The selected model's texture index.
    texture: usize,
    /// The models' rendered thumbnails (w, h, rgba), parallel to `models`.
    previews: Vec<(u32, u32, Vec<u8>)>,
}

impl PlayerModelScreen {
    fn slot_origin(width: i32, height: i32, index: usize, count: usize) -> [i32; 2] {
        if count == 0 {
            return [width / 2, height / 2 - RADIUS];
        }
        let angle = -std::f64::consts::FRAC_PI_2
            + index as f64 * std::f64::consts::TAU / count as f64;
        [
            width / 2 + (angle.cos() * f64::from(RADIUS)) as i32 - SLOT / 2,
            height / 2 - RADIUS / 2 + (angle.sin() * f64::from(RADIUS)) as i32 - SLOT / 2,
        ]
    }

    fn hit_test(width: i32, height: i32, count: usize, cursor: (i32, i32)) -> Option<usize> {
        for index in 0..count {
            let [x, y] = Self::slot_origin(width, height, index, count);
            if cursor.0 >= x
                && cursor.0 < x + SLOT
                && cursor.1 >= y
                && cursor.1 < y + SLOT
            {
                return Some(index);
            }
        }
        None
    }
}

/// The action's display label: the pack's own label wins, else known names
/// translate and the rest show raw.
fn action_label(action: &horizon_client::player_models::WheelAction) -> String {
    if !action.label.is_empty() {
        return action.label.clone();
    }
    let name = action.key.as_str();
    match name {
        "beg" => "卖萌".to_owned(),
        "chair" => "坐下".to_owned(),
        "gomoku" => "下五子棋".to_owned(),
        "picnic" => "野餐".to_owned(),
        "game_win" => "胜利".to_owned(),
        "game_lost" => "认输".to_owned(),
        n if n.starts_with("dance") => format!("舞蹈 {}", &n[5..]),
        n if n.starts_with("action") => format!("动作 {}", &n[6..]),
        n if n.starts_with("extra") => format!("彩蛋 {}", &n[5..]),
        other => other.to_owned(),
    }
}

impl Game<'_> {
    pub(super) fn open_player_model_screen(&mut self, tab: Tab) {
        let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let models = horizon_client::player_models::list_models(&root);
        // Each pack's thumbnail, rendered right here (the same software
        // rasterizer the launcher's 3D-skin tab uses): no atlas sprite is
        // involved, the wheel blits the pixels as fill rects.
        let previews = models
            .iter()
            .map(|m| {
                horizon_client::player_models::render_preview_png(&root, &m.file)
                    .and_then(|png| horizon_assets::player_model::decode_png(&png[..]).ok())
                    .unwrap_or((0, 0, Vec::new()))
            })
            .collect();
        // The current selection: the settings' model, if still installed.
        let current = horizon_client::player_models::selected_model(&root)
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
        let selected = models
            .iter()
            .position(|m| Some(&m.file) == current.as_ref())
            .unwrap_or(0);
        // The settings' texture, clamped to the pack's material count. This
        // used to read `textures.min(1) - 1`, which is always 0: reopening the
        // wheel silently reset the material, so a switch looked like it never
        // took.
        let texture = horizon_client::player_models::selected_info(&root)
            .map(|(_, t)| t)
            .unwrap_or(0)
            .min(models.get(selected).map_or(0, |m| m.textures.saturating_sub(1)));
        // The selected pack's own actions (its baked animation list), not
        // whatever pack the player currently wears: opening the wheel right
        // after switching models used to keep the previous pack's list.
        let actions = models.get(selected).map_or_else(Vec::new, |m| {
            horizon_client::player_models::actions_for(&root, &m.file, texture)
        });
        let actions = if actions.is_empty() {
            horizon_client::player_models::action_list()
                .into_iter()
                .map(|key| horizon_client::player_models::WheelAction {
                    key,
                    label: String::new(),
                })
                .collect()
        } else {
            actions
        };
        self.player_model_screen = Some(PlayerModelScreen {
            hovered: None,
            tab,
            actions,
            buttons: Vec::new(),
            action_page: 0,
            selected,
            texture,
            previews,
            models,
        });
        self.screen = Screen::PlayerModel;
        self.keys.clear();
        self.left = false;
        self.right = false;
        self.mining = None;
        self.capture(false);
    }

    /// Applies the wheel's selection and closes. The model swap re-reads the
    /// folder right away; the texture needs an atlas rebuild (next join).
    pub(super) fn close_player_model_screen(&mut self, apply: bool) {
        let screen = self.player_model_screen.take();
        self.screen = Screen::Playing;
        self.capture(true);
        let Some(screen) = screen else {
            return;
        };
        if !apply || screen.tab != Tab::Models {
            return;
        }
        let Some(model) = screen.models.get(screen.selected) else {
            return;
        };
        let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let previous = horizon_client::player_models::selected_info(&root);
        // The atlas holds ONE `horizon/cpm` sprite — whichever pack was
        // selected when it was built. Switching packs without rebuilding it
        // leaves the new model sampling the old pack's pixels (the shredded
        // purple sheets and the floating texture card), so any change of
        // pack or material has to rebuild the atlas.
        let changed = previous
            .as_ref()
            .is_none_or(|(file, t)| *file != model.file || *t != screen.texture);
        horizon_client::player_models::write_settings(&root, true, &model.file, screen.texture);
        // Rebuild the player's model with the new pack: drop the parse cache
        // (it is keyed on the resource root, not the cwd the packs live in),
        // re-read right away and hand the fresh type to the next tick.
        super::perspective::invalidate_player_type(&root);
        if let Some(kind) = super::perspective::player_type(&root) {
            super::perspective::set_player_type_override(kind);
        }
        self.player_model = None;
        // The wheel and the state machine follow the new pack at once: the
        // previous pack's animations drove bones that no longer exist (and
        // its action list stayed on the wheel).
        horizon_client::player_models::refresh_loaded(&root);
        // A new pack or material is new pixels for that one sprite: the pack
        // reload rebuilds the shared atlas (with the loading overlay) right
        // away instead of leaving the old texture until the next join.
        if changed {
            self.reload_packs();
        }
    }

    /// 「恢复史蒂夫」：关掉自定义模型，原版皮肤立刻顶上来。玩家类型缓存
    /// 里存的是烘好的 Part（见 `load_player_type`），所以还得请掉缓存、
    /// 立刻重建一次，再把手上的类型换掉。
    pub(super) fn apply_vanilla_player(&mut self) {
        let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        horizon_client::player_models::set_transform(&root, false);
        super::perspective::invalidate_player_type(&root);
        if let Some(kind) = super::perspective::player_type(&root) {
            super::perspective::set_player_type_override(kind);
        }
        self.player_model = None;
        self.reload_packs();
    }

    /// 左键 / 触屏点按：模型页点中槽位即应用并关闭；动作页点中条目即播放
    /// 并关闭，点「停止动作」清掉当前动作。点空处不动。
    pub(super) fn player_model_click(&mut self) {
        let hovered = self
            .player_model_screen
            .as_ref()
            .and_then(|s| s.hovered);
        if let Some(index) = hovered {
            if let Some(screen) = &mut self.player_model_screen {
                screen.selected = index;
            }
            self.close_player_model_screen(true);
            return;
        }
        let hit = self
            .player_model_screen
            .as_ref()
            .map(|s| {
                let c = (self.cursor.0 as i32, self.cursor.1 as i32);
                s.buttons
                    .iter()
                    .find(|b| c.0 >= b.rect[0] && c.0 < b.rect[0] + b.rect[2] && c.1 >= b.rect[1] && c.1 < b.rect[1] + b.rect[3])
                    .map(|b| b.kind)
            })
            .unwrap_or(None);
        match hit {
            Some(WheelButtonKind::Action(index)) => {
                if let Some(action) = self
                    .player_model_screen
                    .as_ref()
                    .and_then(|s| s.actions.get(index))
                {
                    horizon_client::player_models::trigger_action(&action.key);
                }
                self.close_player_model_screen(false);
            }
            Some(WheelButtonKind::Stop) => {
                horizon_client::player_models::clear_action();
                self.close_player_model_screen(false);
            }
            Some(WheelButtonKind::Vanilla) => {
                self.apply_vanilla_player();
                self.close_player_model_screen(false);
            }
            Some(WheelButtonKind::PagePrev) => {
                if let Some(screen) = &mut self.player_model_screen {
                    screen.action_page = screen.action_page.saturating_sub(1);
                }
            }
            Some(WheelButtonKind::PageNext) => {
                let per_page = 8;
                if let Some(screen) = &mut self.player_model_screen {
                    let pages = screen.actions.len().div_ceil(per_page).max(1);
                    if screen.action_page + 1 < pages {
                        screen.action_page += 1;
                    }
                }
            }
            Some(kind @ (WheelButtonKind::TabModels | WheelButtonKind::TabActions)) => {
                if let Some(screen) = &mut self.player_model_screen {
                    screen.tab = match kind {
                        WheelButtonKind::TabModels => Tab::Models,
                        _ => {
                            screen.action_page = 0;
                            Tab::Actions
                        }
                    };
                }
            }
            None => {}
        }
    }

    pub(super) fn draw_player_model_screen(&mut self, batch: &mut HudBatch) {
        let (w, h) = (batch.width as i32, batch.height as i32);
        let Some(screen) = &mut self.player_model_screen else {
            return;
        };
        horizon_client::widgets::begin_screen(|| {
            horizon_client::widgets::ScreenInfo::new("player_model", "")
        });
        let cursor = (self.cursor.0 as i32, self.cursor.1 as i32);
        let count = screen.models.len();
        screen.hovered = PlayerModelScreen::hit_test(w, h, count, cursor);
        let atlas = &self.gui.atlas;
        // Dim the world behind the wheel.
        batch.fill(atlas, 0.0, 0.0, w as f32, h as f32, 0x8C000000);
        // The two tabs (模型 / 动作), above the wheel.
        screen.buttons.clear();
        let tabs: [(WheelButtonKind, &str); 2] = [
            (WheelButtonKind::TabModels, "playerModel.tabModels"),
            (WheelButtonKind::TabActions, "playerModel.tabActions"),
        ];
        let tab_w = 84i32;
        let tab_h = 14i32;
        let tab_y = h / 2 - RADIUS - 46;
        for (i, (kind, key)) in tabs.iter().enumerate() {
            let tx = w / 2 - tab_w - 4 + i as i32 * (tab_w + 8);
            let active = match kind {
                WheelButtonKind::TabModels => screen.tab == Tab::Models,
                _ => screen.tab == Tab::Actions,
            };
            batch.fill(atlas, tx as f32, tab_y as f32, tab_w as f32, tab_h as f32, 0xB4000000);
            let border = if active { 0xFF55FFFFu32 } else { 0x88FFFFFFu32 };
            for (bx, by, bw, bh) in [
                (tx, tab_y, tab_w, 1),
                (tx, tab_y + tab_h - 1, tab_w, 1),
                (tx, tab_y, 1, tab_h),
                (tx + tab_w - 1, tab_y, 1, tab_h),
            ] {
                batch.fill(atlas, bx as f32, by as f32, bw as f32, bh as f32, border);
            }
            let label = Text::translate(key, &[]);
            let tw = HudBatch::text_width(atlas, &label.plain());
            batch.text_styled(
                atlas,
                &label,
                (tx + tab_w / 2) as f32 - tw as f32 / 2.0,
                (tab_y + 3) as f32,
                0xFFFFFFFF,
                true,
            );
            screen.buttons.push(WheelButton { kind: *kind, rect: [tx, tab_y, tab_w, tab_h] });
        }
        // The wheel's slots with the packs' preview thumbnails (模型页).
        if screen.tab == Tab::Models {
        for index in 0..count {
            let [x, y] = PlayerModelScreen::slot_origin(w, h, index, count);
            let selected = index == screen.selected;
            let hovered = screen.hovered == Some(index);
            let border = if selected {
                0xFF55FFFFu32
            } else if hovered {
                0xFFFFFFFFu32
            } else {
                0x88FFFFFFu32
            };
            // Slot backing and a 1px border drawn as four thin fills.
            batch.fill(
                atlas,
                (x - 1) as f32,
                (y - 1) as f32,
                (SLOT + 2) as f32,
                (SLOT + 2) as f32,
                0xB4000000,
            );
            for (bx, by, bw, bh) in [
                (x - 1, y - 1, SLOT + 2, 1),
                (x - 1, y + SLOT, SLOT + 2, 1),
                (x - 1, y, 1, SLOT),
                (x + SLOT, y, 1, SLOT),
            ] {
                batch.fill(atlas, bx as f32, by as f32, bw as f32, bh as f32, border);
            }
            if let Some((tw, th, rgba)) = screen.previews.get(index)
                && *tw > 0
                && *th > 0
            {
                // Blit the rendered thumbnail as fill rects (run-length
                // encoded rows, like the launcher's 3D-skin tab), centered
                // and scaled into the slot.
                let s = SLOT as f32 / (*tw).max(*th) as f32;
                let ox = x as f32 + (SLOT as f32 - *tw as f32 * s) / 2.0;
                let oy = y as f32 + (SLOT as f32 - *th as f32 * s) / 2.0;
                for py in 0..*th as usize {
                    let mut px = 0usize;
                    while px < *tw as usize {
                        let i = (py * *tw as usize + px) * 4;
                        if rgba[i + 3] == 0 {
                            px += 1;
                            continue;
                        }
                        let color = [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]];
                        let start = px;
                        while px < *tw as usize {
                            let j = (py * *tw as usize + px) * 4;
                            if rgba[j + 3] == 0 || rgba[j..j + 3] != color[..3] {
                                break;
                            }
                            px += 1;
                        }
                        batch.fill(
                            atlas,
                            ox + start as f32 * s,
                            oy + py as f32 * s,
                            (px - start) as f32 * s,
                            s,
                            horizon_client::hud::rgba(
                                color[0],
                                color[1],
                                color[2],
                                color[3],
                            ),
                        );
                    }
                }
            }
        }
        // The selected pack's name and the hint line.
        if let Some(model) = screen.models.get(screen.selected) {
            let name = Text::literal(&model.name);
            let tw = HudBatch::text_width(atlas, &name.plain());
            batch.text_styled(
                atlas,
                &name,
                w as f32 / 2.0 - tw / 2.0,
                (h / 2 - RADIUS - 26) as f32,
                0xFFFFFFFF,
                true,
            );
            let textures = model.textures.max(1);
            let tex = screen.texture.min(textures - 1);
            let hint = Text::translate(
                "playerModel.wheelHint",
                &[
                    keybinds::display_name(self.settings.keys.get("key.playerModel"))
                        .with_color(0xFF55FFFF),
                ],
            );
            let hw = HudBatch::text_width(atlas, &hint.plain());
            batch.text_styled(
                atlas,
                &hint,
                w as f32 / 2.0 - hw / 2.0,
                (h / 2 + RADIUS + 10) as f32,
                0xFFA0A0A0,
                true,
            );
            // 取消变身：点一下就换回原版史蒂夫，模型与材质的选择留着，
            // 想再变身点回槽位即可。
            let vw = 82i32;
            let vh = 16i32;
            let vx = w / 2 - vw / 2;
            let vy = h / 2 + RADIUS + 24;
            let hot_vanilla = cursor.0 >= vx
                && cursor.1 >= vy
                && cursor.0 < vx + vw
                && cursor.1 < vy + vh;
            batch.fill(
                atlas,
                vx as f32,
                vy as f32,
                vw as f32,
                vh as f32,
                if hot_vanilla { 0xE0304050 } else { 0xB4000000 },
            );
            for (rx, ry, rw, rh) in [
                (vx, vy, vw, 1),
                (vx, vy + vh - 1, vw, 1),
                (vx, vy, 1, vh),
                (vx + vw - 1, vy, 1, vh),
            ] {
                batch.fill(
                    atlas,
                    rx as f32,
                    ry as f32,
                    rw as f32,
                    rh as f32,
                    if hot_vanilla { 0xFF55FFFFu32 } else { 0x88FFFFFFu32 },
                );
            }
            let vanilla = Text::translate("playerModel.vanillaModel", &[]);
            let nl = HudBatch::text_width(atlas, &vanilla.plain());
            batch.text_styled(
                atlas,
                &vanilla,
                (vx + vw / 2) as f32 - nl as f32 / 2.0,
                (vy + 4) as f32,
                0xFFFFFFFF,
                true,
            );
            screen.buttons.push(WheelButton {
                kind: WheelButtonKind::Vanilla,
                rect: [vx, vy, vw, vh],
            });
            // The texture strip: one cell per material of the selected pack.
            if textures > 1 {
                let strip_w = textures as i32 * (TEX_SLOT + 4) - 4;
                let sx = w / 2 - strip_w / 2;
                let sy = h / 2 + RADIUS / 2 + 6;
                for i in 0..textures {
                    let cx = sx + i as i32 * (TEX_SLOT + 4);
                    batch.fill(
                        atlas,
                        cx as f32,
                        sy as f32,
                        TEX_SLOT as f32,
                        TEX_SLOT as f32,
                        0xB4000000,
                    );
                    let border = if i == tex { 0xFF55FFFFu32 } else { 0x88FFFFFFu32 };
                    for (bx, by, bw, bh) in [
                        (cx, sy, TEX_SLOT, 1),
                        (cx, sy + TEX_SLOT - 1, TEX_SLOT, 1),
                        (cx, sy, 1, TEX_SLOT),
                        (cx + TEX_SLOT - 1, sy, 1, TEX_SLOT),
                    ] {
                        batch.fill(atlas, bx as f32, by as f32, bw as f32, bh as f32, border);
                    }
                    let label = Text::literal(&(i + 1).to_string());
                    batch.text_styled(
                        atlas,
                        &label,
                        (cx + TEX_SLOT / 2 - 3) as f32,
                        (sy + 4) as f32,
                        0xFFFFFFFF,
                        true,
                    );
                }
                // Hovering the strip picks the material.
                if cursor.1 >= sy
                    && cursor.1 < sy + TEX_SLOT
                    && cursor.0 >= sx
                    && cursor.0 < sx + strip_w
                {
                    screen.texture =
                        ((cursor.0 - sx) / (TEX_SLOT + 4)).clamp(0, textures as i32 - 1) as usize;
                }
            }
        } else {
            let empty = Text::translate("playerModel.wheelEmpty", &[]);
            let ew = HudBatch::text_width(atlas, &empty.plain());
            batch.text_styled(
                atlas,
                &empty,
                w as f32 / 2.0 - ew / 2.0,
                (h / 2) as f32,
                0xFFA0A0A0,
                true,
            );
        }
        } else {
            // 动作页：YSM 式环形轮盘。八个动作绕一圈，中心是「停止动作」，
            // 多于八个分页（底部 ◀ 页码 ▶）。环半径与模型页一致，切换页签
            // 时视觉不跳。
            const PER_PAGE: usize = 8;
            let pages = screen.actions.len().div_ceil(PER_PAGE).max(1);
            let page = screen.action_page.min(pages - 1);
            let start = page * PER_PAGE;
            if screen.actions.is_empty() {
                let empty = Text::translate("playerModel.actionsEmpty", &[]);
                let ew = HudBatch::text_width(atlas, &empty.plain());
                batch.text_styled(
                    atlas,
                    &empty,
                    w as f32 / 2.0 - ew as f32 / 2.0,
                    (h / 2) as f32,
                    0xFFA0A0A0,
                    true,
                );
            }
            // Eight 62px cells need a ring of at least ~79px radius; the
            // radius also has to fit the shorter screen edge.
            let cell_w = 62i32;
            let cell_h = 16i32;
            let radius = ((w.min(h) as f32) * 0.34).clamp(80.0, 120.0) as i32;
            let (cx, cy) = (w / 2, h / 2 - radius / 6);
            // The ring: same -90° start as the model wheel, eight spokes.
            for i in 0..PER_PAGE {
                let Some(index) = (start + i).checked_add(0).filter(|n| *n < screen.actions.len())
                else {
                    break;
                };
                let action = &screen.actions[index];
                let angle = -std::f64::consts::FRAC_PI_2
                    + i as f64 * std::f64::consts::TAU / PER_PAGE as f64;
                let px = cx + (angle.cos() * f64::from(radius)) as i32;
                let py = cy + (angle.sin() * f64::from(radius)) as i32;
                let bx = px - cell_w / 2;
                let by = py - cell_h / 2;
                let hot = cursor.0 >= bx
                    && cursor.0 < bx + cell_w
                    && cursor.1 >= by
                    && cursor.1 < by + cell_h;
                batch.fill(
                    atlas,
                    bx as f32,
                    by as f32,
                    cell_w as f32,
                    cell_h as f32,
                    if hot { 0xE0304050 } else { 0xB4000000 },
                );
                for (rx, ry, rw, rh) in [
                    (bx, by, cell_w, 1),
                    (bx, by + cell_h - 1, cell_w, 1),
                    (bx, by, 1, cell_h),
                    (bx + cell_w - 1, by, 1, cell_h),
                ] {
                    batch.fill(
                        atlas,
                        rx as f32,
                        ry as f32,
                        rw as f32,
                        rh as f32,
                        if hot { 0xFF55FFFFu32 } else { 0x88FFFFFFu32 },
                    );
                }
                let text = Text::literal(action_label(action));
                let tw = HudBatch::text_width(atlas, &text.plain());
                batch.text_styled(
                    atlas,
                    &text,
                    (bx + cell_w / 2) as f32 - tw as f32 / 2.0,
                    (by + 5) as f32,
                    0xFFFFFFFF,
                    true,
                );
                screen.buttons.push(WheelButton {
                    kind: WheelButtonKind::Action(index),
                    rect: [bx, by, cell_w, cell_h],
                });
            }
            // Centre: 停止动作.
            let stop_w = 70i32;
            let stop_h = 20i32;
            let sx = cx - stop_w / 2;
            let sy = cy - stop_h / 2;
            let hot_stop = cursor.0 >= sx
                && cursor.0 < sx + stop_w
                && cursor.1 >= sy
                && cursor.1 < sy + stop_h;
            batch.fill(
                atlas,
                sx as f32,
                sy as f32,
                stop_w as f32,
                stop_h as f32,
                if hot_stop { 0xE0502020 } else { 0xC0201414 },
            );
            for (rx, ry, rw, rh) in [
                (sx, sy, stop_w, 1),
                (sx, sy + stop_h - 1, stop_w, 1),
                (sx, sy, 1, stop_h),
                (sx + stop_w - 1, sy, 1, stop_h),
            ] {
                batch.fill(
                    atlas,
                    rx as f32,
                    ry as f32,
                    rw as f32,
                    rh as f32,
                    if hot_stop { 0xFFFF8888u32 } else { 0xFFFF5555u32 },
                );
            }
            let stop = Text::translate("playerModel.stopAction", &[]);
            let sw = HudBatch::text_width(atlas, &stop.plain());
            batch.text_styled(
                atlas,
                &stop,
                (sx + stop_w / 2) as f32 - sw as f32 / 2.0,
                (sy + 6) as f32,
                0xFFFFFFFF,
                true,
            );
            screen.buttons.push(WheelButton {
                kind: WheelButtonKind::Stop,
                rect: [sx, sy, stop_w, stop_h],
            });
            // Paging arrows at the bottom when there are more than eight.
            if pages > 1 {
                let py = cy + radius + 14;
                for (kind, label, px) in [
                    (WheelButtonKind::PagePrev, "<", w / 2 - 46),
                    (WheelButtonKind::PageNext, ">", w / 2 + 34),
                ] {
                    let enabled = match kind {
                        WheelButtonKind::PagePrev => page > 0,
                        _ => page + 1 < pages,
                    };
                    batch.fill(atlas, px as f32, py as f32, 12.0, 14.0, 0xB4000000);
                    let arrow = Text::literal(label);
                    batch.text_styled(
                        atlas,
                        &arrow,
                        (px + 4) as f32,
                        (py + 3) as f32,
                        if enabled { 0xFFFFFFFF } else { 0xFF606060 },
                        true,
                    );
                    if enabled {
                        screen.buttons.push(WheelButton {
                            kind,
                            rect: [px, py, 12, 14],
                        });
                    }
                }
                let counter = Text::literal(format!("{}/{}", page + 1, pages));
                let cw = HudBatch::text_width(atlas, &counter.plain());
                batch.text_styled(
                    atlas,
                    &counter,
                    w as f32 / 2.0 - cw as f32 / 2.0,
                    py as f32 + 3.0,
                    0xFFA0A0A0,
                    true,
                );
            }
            let hint = Text::translate("playerModel.wheelHintActions", &[]);
            let hw = HudBatch::text_width(atlas, &hint.plain());
            batch.text_styled(
                atlas,
                &hint,
                w as f32 / 2.0 - hw as f32 / 2.0,
                (cy + radius + 34) as f32,
                0xFFA0A0A0,
                true,
            );
        }
    }
}
