//! Explicit, scrollable selections. Values never cycle on repeated button taps.
use super::{
    Launcher,
    pages::{Action, button},
};
use crate::catalog::RendererMode;
use horizon_client::ui_theme::{Face, Ui};

pub(super) enum Kind {
    Aspect,
    DisplayHz,
    CacheBudget,
    WorkerLimit,
    Renderer,
    RenderScale,
    Profile,
    Orientation,
    InputHost,
    UiScale,
    ControlProfile,
}
pub(super) struct Chooser {
    title: &'static str,
    help: &'static str,
    /// 多行帮助文本（逐行渲染，用于声明弹窗这类需要排版的场景）。
    multiline_help: Vec<&'static str>,
    device: Option<String>,
    choices: Vec<(String, String, Action, bool)>,
    pub scroll: f32,
}
impl Launcher {
    pub(super) fn open_chooser(&mut self, kind: Kind) {
        crate::platform::hide();
        self.catalog.editing = None;
        let device = if matches!(kind, Kind::Renderer) {
            self.renderer.as_ref().map(|r| {
                format!(
                    "GPU：{} / {} {}",
                    r.info.name, r.info.driver, r.info.driver_info
                )
            })
        } else {
            None
        };
        let c = &self.catalog.config;
        let (title, help, choices) = match kind {
            Kind::Aspect => (
                "宽高比",
                "完整显示画面不拉伸，触控坐标跟随画面。竖屏时交换宽高。",
                [
                    crate::display::Aspect::Native,
                    crate::display::Aspect::Wide,
                    crate::display::Aspect::Classic,
                    crate::display::Aspect::Square,
                ]
                .into_iter()
                .map(|a| {
                    (
                        a.label().into(),
                        String::new(),
                        Action::ChooseAspect(a),
                        c.screen.aspect == a,
                    )
                })
                .collect(),
            ),
            Kind::CacheBudget => (
                "可复用内存缓存",
                "已解码音频（80%）与空闲网格缓冲（20%），会按相同质量重建。预算越小 CPU 开销越大。不是总内存/显存。下次启动游戏时生效。",
                [0, 16, 32, 64, 128, 256]
                    .into_iter()
                    .map(|mib| {
                        (
                            if mib == 0 {
                                "引擎默认（80 MiB）".into()
                            } else {
                                format!("{mib} MiB")
                            },
                            String::new(),
                            Action::ChooseCacheBudget(mib),
                            c.cache_budget_mib == mib,
                        )
                    })
                    .collect(),
            ),
            Kind::WorkerLimit => (
                "区块线程上限",
                "生成与网格构建线程的上限，不是所有应用线程。调低可减少 CPU 争用但会拖慢加载。下次启动游戏时生效。",
                (0..=64u32)
                    .map(|limit| {
                        (
                            if limit == 0 {
                                "不额外限制".into()
                            } else {
                                format!("{limit} 个线程")
                            },
                            String::new(),
                            Action::ChooseWorkerLimit(limit),
                            c.worker_limit == limit,
                        )
                    })
                    .collect(),
            ),
            Kind::DisplayHz => (
                "屏幕刷新率",
                "这是安卓显示偏好，不是游戏帧率上限。不支持的刷新率可能被忽略。",
                crate::device_preferences::DISPLAY_RATES
                    .into_iter()
                    .map(|hz| {
                        (
                            if hz == 0 {
                                "系统默认".into()
                            } else {
                                format!("{hz} Hz")
                            },
                            String::new(),
                            Action::ChooseDisplayHz(hz),
                            c.device.display_hz == hz,
                        )
                    })
                    .collect(),
            ),
            Kind::RenderScale => {
                let mut choices: Vec<_> = crate::display::RenderScale::PRESETS
                    .into_iter()
                    .map(|percent| {
                        (
                            format!("{percent}%"),
                            if percent == 100 {
                                "原生分辨率".into()
                            } else {
                                "降低整体画面分辨率".into()
                            },
                            Action::ChooseRenderScale(crate::display::RenderScale::new(percent)),
                            c.render_scale.percent() == percent,
                        )
                    })
                    .collect();
                choices.push((
                    "自定义百分比…".into(),
                    "请输入 25 到 100 的数值".into(),
                    Action::Field("render_scale"),
                    false,
                ));
                (
                    "客户端分辨率",
                    "小屏幕也能保持可用的菜单大小。",
                    choices,
                )
            }
            Kind::InputHost => (
                "输入模式",
                "鼠标模式使用物理键盘；触屏模式保留安卓键盘。",
                crate::catalog::InputHost::ALL
                    .into_iter()
                    .map(|mode| {
                        (
                            mode.label().into(),
                            String::new(),
                            Action::ChooseInputHost(mode),
                            c.input_host == mode,
                        )
                    })
                    .collect(),
            ),
            Kind::Orientation => (
                "屏幕方向",
                "对启动器和游戏生效。不会更改触控绑定。",
                crate::catalog::Orientation::ALL
                    .into_iter()
                    .map(|mode| {
                        (
                            mode.label().into(),
                            String::new(),
                            Action::ChooseOrientation(mode),
                            c.orientation == mode,
                        )
                    })
                    .collect(),
            ),
            Kind::Renderer => (
                "渲染模式",
                "使用系统 GPU 驱动。下次启动游戏时生效。",
                vec![
                    (
                        "OpenGL ES / 实验性".into(),
                        "面向旧款 GPU。需要 ES 3.0+ 与足够的驱动上限。".into(),
                        Action::ChooseRenderer(RendererMode::OpenGl),
                        c.renderer == RendererMode::OpenGl,
                    ),
                    (
                        "Vulkan / GPU 批处理".into(),
                        "优化过的 Vulkan 管线：逐页绘制合批与高吞吐区块流送。".into(),
                        Action::ChooseRenderer(RendererMode::Vulkan),
                        c.renderer == RendererMode::Vulkan,
                    ),
                    (
                        "Vulkan / 兼容模式".into(),
                        "标准 Vulkan 渲染路径，逐区块直接绘制。"
                            .into(),
                        Action::ChooseRenderer(RendererMode::VulkanCompatibility),
                        c.renderer == RendererMode::VulkanCompatibility,
                    ),
                ],
            ),
            Kind::Profile => {
                let mut ids = vec![0, c.profile];
                ids.extend(
                    std::fs::read_dir(self.home.join("profiles"))
                        .into_iter()
                        .flatten()
                        .flatten()
                        .filter(|e| e.path().is_dir())
                        .filter_map(|e| {
                            e.file_name()
                                .to_str()?
                                .strip_prefix("profile-")?
                                .parse::<u32>()
                                .ok()
                        }),
                );
                ids.sort_unstable();
                ids.dedup();
                let next = ids.last().copied().unwrap_or(0).checked_add(1);
                let mut choices: Vec<_> = ids
                    .into_iter()
                    .map(|id| {
                        (
                            format!("档案 {id}"),
                            if id == 0 {
                                "默认档案".into()
                            } else {
                                "存档、模组与资源包相互独立".into()
                            },
                            Action::ChooseProfile(id),
                            id == c.profile,
                        )
                    })
                    .collect();
                if let Some(id) = next {
                    choices.push((
                        "创建档案".into(),
                        format!("创建档案 {id}"),
                        Action::ChooseProfile(id),
                        false,
                    ));
                }
                (
                    "选择档案",
                    "每个档案有独立的游戏设置与存档。",
                    choices,
                )
            }
            Kind::UiScale => (
                "启动器界面缩放",
                "让启动器适配手机 DPI。如果触控按钮和文字太大或太小，可以调高或调低。",
                [
                    (
                        0u32,
                        "自动（手机 DPI）",
                        "自动匹配屏幕 DPI 密度",
                    ),
                    (85, "85%（紧凑）", "一屏显示更多行"),
                    (100, "100%（默认 DPI）", "标准手机 DPI 缩放"),
                    (115, "115%（大）", "更大的按钮、卡片与文字"),
                    (
                        130,
                        "130%（特大）",
                        "高 DPI 手机上的最佳触控体验",
                    ),
                ]
                .into_iter()
                .map(|(pct, title, desc)| {
                    (
                        title.into(),
                        desc.into(),
                        Action::ChooseUiScale(pct),
                        c.ui_scale_percent == pct,
                    )
                })
                .collect(),
            ),
            Kind::ControlProfile => {
                let game_home = c.game_home(&self.home);
                let touch = horizon_client::touch::Settings::load_from(
                    &game_home.join("horizon-touch.json"),
                );
                let style = touch.control_style;
                let presets = horizon_client::touch::Settings::list_presets(&game_home, style);
                let choices = presets
                    .into_iter()
                    .enumerate()
                    .map(|(idx, meta)| {
                        let active = touch.preset_id.eq_ignore_ascii_case(&meta.id);
                        let mut title = meta.name.clone();
                        if meta.is_default {
                            title.push_str("  （受保护的默认）");
                        }
                        let sub = format!(
                            "{}  •  controls/{}/{}.json",
                            meta.description,
                            style.storage_subdir(),
                            meta.id
                        );
                        (title, sub, Action::ChooseControlProfile(idx as u32), active)
                    })
                    .collect();
                (
                    match style {
                        horizon_client::touch::ControlStyle::Mcpe => "MCPE 控制配置",
                        horizon_client::touch::ControlStyle::GoyControl => {
                            "GoyControl 控制配置"
                        }
                    },
                    match style {
                        horizon_client::touch::ControlStyle::Mcpe => {
                            "独立 MCPE 配置存放在 controls/mcpe/。默认配置受保护。"
                        }
                        horizon_client::touch::ControlStyle::GoyControl => {
                            "独立 GoyControl 配置存放在 controls/goycontrol/。默认配置受保护。"
                        }
                    },
                    choices,
                )
            }
        };
        self.chooser = Some(Chooser {
            title,
            help,
            multiline_help: Vec::new(),
            choices,
            device,
            scroll: 0.0,
        });
    }
}
impl Chooser {
    pub(super) fn confirm_delete(name: String) -> Self {
        Self {
            title: "移除附加内容？",
            help: "只从该档案删除所选文件。此操作无法撤销。",
            multiline_help: Vec::new(),
            device: Some(name),
            choices: vec![
                (
                    "移除".into(),
                    String::new(),
                    Action::ConfirmDeleteLocal,
                    false,
                ),
                ("取消".into(), String::new(), Action::Dismiss, true),
            ],
            scroll: 0.0,
        }
    }

    /// 首次启动时的启动器声明（免责声明），按原始分行排版。
    pub(super) fn declaration() -> Self {
        Self {
            title: "温馨提示",
            help: "",
            multiline_help: vec![
                "本启动器基于项目二改",
                "可能会出现一些不稳定性",
                "本项目完全公益但并不开源",
                "本项目作者为 B 站小白爱补钙",
                "使用 AI 来进行完成",
                "本项目使用的内置模型与功能均来自项目：",
                "Yes Steve Model / WineFoxModel（酒狐模型）",
                "作者：TartaricAcid",
                "链接：github.com/TartaricAcid/WineFoxModel",
                "",
                "非商用，纯热爱发电",
                "要是买来的话，那你就被骗了，大傻子",
            ],
            device: None,
            choices: vec![(
                "我知道了，不再显示".into(),
                String::new(),
                Action::DismissDeclaration,
                false,
            )],
            scroll: 0.0,
        }
    }

    pub(super) fn navigation(&self, width: f32) -> Vec<([f32; 4], Action)> {
        self.choices
            .iter()
            .enumerate()
            .map(|(i, (_, _, action, _))| {
                (
                    [
                        32.0,
                        137.0 + i as f32 * 84.0 - self.scroll,
                        width - 64.0,
                        76.0,
                    ],
                    *action,
                )
            })
            .collect()
    }
    pub(super) fn viewport(&self, height: f32) -> [f32; 3] {
        [
            137.0,
            height - 100.0,
            (self.choices.len() as f32 * 84.0 - (height - 237.0)).max(0.0),
        ]
    }
    pub fn draw(&mut self, ui: &mut Ui, targets: &mut Vec<([f32; 4], Action)>, modal_anim: f32) {
        let (w, h) = (ui.w, ui.h);
        let theme = horizon_client::ui_theme::active_theme();
        let slide = (1.0 - modal_anim.clamp(0.0, 1.0)) * 16.0;
        targets.clear();
        ui.draw_theme_background(0.0, 0.0, w, h, &theme);
        ui.text(
            Face::SemiBold,
            24.0,
            32.0,
            32.0 + slide,
            theme.text_primary,
            self.title,
        );
        let multiline = !self.multiline_help.is_empty();
        if multiline {
            for (i, line) in self.multiline_help.iter().enumerate() {
                let fitted = ui.fit_text(Face::Regular, 13.5, line, w - 64.0);
                ui.text(
                    Face::Regular,
                    13.5,
                    32.0,
                    70.0 + i as f32 * 19.0 + slide,
                    theme.text_muted,
                    &fitted,
                );
            }
        } else {
            ui.text(
                Face::Regular,
                14.0,
                32.0,
                74.0 + slide,
                theme.text_muted,
                self.help,
            );
        }
        if let Some(device) = &self.device {
            ui.hint_line(32.0, 100.0 + slide, w - 64.0, device, theme.accent);
        }
        let top = if multiline {
            70.0 + self.multiline_help.len() as f32 * 19.0 + 18.0 + slide
        } else {
            132.0 + slide
        };
        let bottom = h - 94.0;
        self.scroll = self.scroll.clamp(
            0.0,
            (self.choices.len() as f32 * 84.0 - (bottom - top)).max(0.0),
        );
        ui.set_clip(Some([0.0, top, w, (bottom - top).max(1.0)]));
        for (i, (label, detail, action, selected)) in self.choices.iter().enumerate() {
            let y = top + i as f32 * 84.0 - self.scroll;
            if y + 76.0 < top || y > bottom {
                continue;
            }
            ui.rounded(
                32.0,
                y,
                w - 64.0,
                74.0,
                14.0,
                if *selected {
                    theme.surface_selected
                } else {
                    theme.surface
                },
            );
            ui.stroke_rounded(
                32.0,
                y,
                w - 64.0,
                74.0,
                14.0,
                if *selected { 1.6 } else { 1.0 },
                if *selected {
                    theme.accent
                } else {
                    theme.border
                },
            );
            ui.stroke_rounded(
                48.0,
                y + 26.0,
                22.0,
                22.0,
                11.0,
                1.8,
                if *selected {
                    theme.accent
                } else {
                    theme.text_muted
                },
            );
            if *selected {
                ui.rounded(53.0, y + 31.0, 12.0, 12.0, 6.0, theme.accent);
            }
            ui.text(
                Face::SemiBold,
                17.0,
                84.0,
                y + 11.0,
                theme.text_primary,
                label,
            );
            ui.text(
                Face::Regular,
                13.0,
                84.0,
                y + 40.0,
                theme.text_muted,
                detail,
            );
            if let Some(c) = ui.clip_rect([32.0, y, w - 64.0, 74.0]) {
                targets.push((c, *action));
            }
        }
        ui.set_clip(None);
        button(
            ui,
            targets,
            [32.0, h - 74.0, 160.0, 44.0],
            "取消",
            Action::Dismiss,
            false,
        );
    }
}
