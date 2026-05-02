//! `ClientboundUpdateAdvancements` 包到 ECS 的接口。
//!
//! 协议层 `azalea_protocol::packets::game::c_update_advancements::ClientboundUpdateAdvancements`
//! 是 vanilla 增量包：`reset` 整盘清零；`added` 新增 / 覆盖 advancement 定
//! 义（含 parent_id / display / requirements / sends_telemetry_event）；
//! `removed` 按 id 删；`progress` 写每个 criterion 的完成时间戳。本插件
//! 把这套 fold 进 [`Advancements`] **per-player Component**——下游 UI
//! 只读 `&Advancements`，不直接处理 packet。
//!
//! ## 为什么是 Component 不是 Resource
//!
//! 与 boss_bar / scoreboard 同理：azalea swarm 一个 App 跑多 client，
//! advancement 进度是每连接私有；Resource 全局会串台。

use std::collections::{HashMap, HashSet};

use azalea_protocol::packets::game::c_update_advancements::{
    Advancement, AdvancementHolder, AdvancementProgress, ClientboundUpdateAdvancements,
};
use azalea_registry::identifier::Identifier;
use bevy_app::{App, Plugin};
use bevy_ecs::{prelude::*, system::SystemState};

pub struct AdvancementsPlugin;

impl Plugin for AdvancementsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<AdvancementsUpdated>();
    }
}

/// **Per-LocalEntity** 的 advancement 状态。`tree` 是 advancement 定义索引
/// （一份 advancement 可有 parent → 形成 tree），`progress` 是每条
/// advancement 当前 criterion 完成情况。
#[derive(Component, Default, Debug, Clone)]
pub struct Advancements {
    tree: HashMap<Identifier, Advancement>,
    progress: HashMap<Identifier, AdvancementProgress>,
}

impl Advancements {
    pub fn get(&self, id: &Identifier) -> Option<&Advancement> {
        self.tree.get(id)
    }

    pub fn progress(&self, id: &Identifier) -> Option<&AdvancementProgress> {
        self.progress.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Identifier, &Advancement)> {
        self.tree.iter()
    }

    pub fn iter_progress(&self) -> impl Iterator<Item = (&Identifier, &AdvancementProgress)> {
        self.progress.iter()
    }

    pub fn len(&self) -> usize {
        self.tree.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tree.is_empty()
    }

    /// 一个 advancement 是否完成：所有 requirements 行至少 1 列有完成时间戳。
    /// vanilla 的"is done"判定。
    pub fn is_done(&self, id: &Identifier) -> bool {
        let Some(adv) = self.tree.get(id) else {
            return false;
        };
        let Some(prog) = self.progress.get(id) else {
            return false;
        };
        adv.requirements.iter().all(|row| {
            row.iter()
                .any(|crit| prog.get(crit).and_then(|p| p.date).is_some())
        })
    }
}

/// advancement 状态变化通知。`entity` 是收到 packet 的 local player。
#[derive(Message, Clone, Debug)]
pub struct AdvancementsUpdated {
    pub entity: Entity,
    /// 本次 packet `reset=true`：之前的整盘进度全清。
    pub reset: bool,
    /// 本次 packet 新增 / 覆盖的 advancement id。
    pub added: HashSet<Identifier>,
    /// 本次 packet 删除的 advancement id。
    pub removed: HashSet<Identifier>,
    /// 本次 packet 进度发生变化的 advancement id。
    pub progress: HashSet<Identifier>,
    /// vanilla `show_advancements`：server 是否要求弹 toast / 打开
    /// advancement UI（false = 静默同步）。UI 据此区分静默 sync 和需要
    /// 弹窗的更新。
    pub show_advancements: bool,
}

/// 单包 `ClientboundUpdateAdvancements` 接入口——在协议 handler 里
/// `as_system::<...>` 调用。
pub fn apply_update_advancements(
    ecs: &mut World,
    player: Entity,
    p: &ClientboundUpdateAdvancements,
) {
    let mut state = SystemState::<(
        Commands,
        Query<&mut Advancements>,
        MessageWriter<AdvancementsUpdated>,
    )>::new(ecs);
    let (mut commands, mut query, mut writer) = state.get_mut(ecs);

    let added_ids: HashSet<Identifier> =
        p.added.iter().map(|h| h.id.clone()).collect();
    let removed_ids: HashSet<Identifier> = p.removed.iter().cloned().collect();
    let progress_ids: HashSet<Identifier> = p.progress.keys().cloned().collect();

    if let Ok(mut advs) = query.get_mut(player) {
        fold_packet(&mut advs, p);
    } else {
        let mut new_advs = Advancements::default();
        fold_packet(&mut new_advs, p);
        commands.entity(player).insert(new_advs);
    }

    writer.write(AdvancementsUpdated {
        entity: player,
        reset: p.reset,
        added: added_ids,
        removed: removed_ids,
        progress: progress_ids,
        show_advancements: p.show_advancements,
    });
    state.apply(ecs);
}

fn fold_packet(advs: &mut Advancements, p: &ClientboundUpdateAdvancements) {
    if p.reset {
        advs.tree.clear();
        advs.progress.clear();
    }
    for AdvancementHolder { id, value } in &p.added {
        advs.tree.insert(id.clone(), value.clone());
    }
    for id in &p.removed {
        advs.tree.remove(id);
        advs.progress.remove(id);
    }
    for (id, prog) in &p.progress {
        advs.progress.insert(id.clone(), prog.clone());
    }
}

#[cfg(test)]
mod tests {
    use azalea_chat::FormattedText;
    use azalea_inventory::ItemStack;
    use azalea_protocol::packets::game::c_update_advancements::{
        AdvancementHolder, CriterionProgress, DisplayInfo, FrameType,
    };
    use bevy_app::App;
    use indexmap::IndexMap;

    use super::*;

    fn id(s: &str) -> Identifier {
        Identifier::new(s)
    }

    fn make_adv(parent: Option<&str>, requirements: Vec<Vec<&str>>) -> Advancement {
        Advancement {
            parent_id: parent.map(id),
            display: Some(Box::new(DisplayInfo {
                title: FormattedText::from("t".to_owned()),
                description: FormattedText::from("d".to_owned()),
                icon: ItemStack::Empty,
                frame: FrameType::Task,
                show_toast: false,
                hidden: false,
                background: None,
                x: 0.0,
                y: 0.0,
            })),
            requirements: requirements
                .into_iter()
                .map(|row| row.into_iter().map(String::from).collect())
                .collect(),
            sends_telemetry_event: false,
        }
    }

    #[test]
    fn add_then_progress_marks_done() {
        let mut app = App::new();
        app.add_plugins(AdvancementsPlugin);
        let player = app.world_mut().spawn_empty().id();

        let mut progress: IndexMap<Identifier, AdvancementProgress> = IndexMap::new();
        let mut crit = AdvancementProgress::new();
        crit.insert(
            "minecraft:got_stone".to_owned(),
            CriterionProgress {
                date: Some(123_456),
            },
        );
        progress.insert(id("minecraft:stone_age"), crit);

        apply_update_advancements(
            app.world_mut(),
            player,
            &ClientboundUpdateAdvancements {
                reset: false,
                added: vec![AdvancementHolder {
                    id: id("minecraft:stone_age"),
                    value: make_adv(None, vec![vec!["minecraft:got_stone"]]),
                }],
                removed: vec![],
                progress,
                show_advancements: true,
            },
        );

        let advs = app.world().entity(player).get::<Advancements>().unwrap();
        assert_eq!(advs.len(), 1);
        assert!(advs.is_done(&id("minecraft:stone_age")));
    }

    #[test]
    fn reset_clears_then_packet_payload_applied() {
        let mut app = App::new();
        app.add_plugins(AdvancementsPlugin);
        let player = app.world_mut().spawn_empty().id();

        // First packet: add A.
        apply_update_advancements(
            app.world_mut(),
            player,
            &ClientboundUpdateAdvancements {
                reset: false,
                added: vec![AdvancementHolder {
                    id: id("minecraft:a"),
                    value: make_adv(None, vec![]),
                }],
                removed: vec![],
                progress: IndexMap::new(),
                show_advancements: true,
            },
        );
        assert_eq!(
            app.world()
                .entity(player)
                .get::<Advancements>()
                .unwrap()
                .len(),
            1
        );

        // Second packet: reset=true + add B.
        apply_update_advancements(
            app.world_mut(),
            player,
            &ClientboundUpdateAdvancements {
                reset: true,
                added: vec![AdvancementHolder {
                    id: id("minecraft:b"),
                    value: make_adv(None, vec![]),
                }],
                removed: vec![],
                progress: IndexMap::new(),
                show_advancements: true,
            },
        );
        let advs = app.world().entity(player).get::<Advancements>().unwrap();
        assert_eq!(advs.len(), 1);
        assert!(advs.get(&id("minecraft:a")).is_none());
        assert!(advs.get(&id("minecraft:b")).is_some());
    }

    #[test]
    fn show_advancements_flag_propagates_to_message() {
        let mut app = App::new();
        app.add_plugins(AdvancementsPlugin);
        let player = app.world_mut().spawn_empty().id();

        // show_advancements=false（静默 sync）→ message 字段也是 false。
        apply_update_advancements(
            app.world_mut(),
            player,
            &ClientboundUpdateAdvancements {
                reset: false,
                added: vec![],
                removed: vec![],
                progress: IndexMap::new(),
                show_advancements: false,
            },
        );
        let msgs: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<AdvancementsUpdated>>()
            .drain()
            .collect();
        assert_eq!(msgs.len(), 1);
        assert!(!msgs[0].show_advancements);

        apply_update_advancements(
            app.world_mut(),
            player,
            &ClientboundUpdateAdvancements {
                reset: false,
                added: vec![],
                removed: vec![],
                progress: IndexMap::new(),
                show_advancements: true,
            },
        );
        let msgs: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<AdvancementsUpdated>>()
            .drain()
            .collect();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].show_advancements);
    }

    #[test]
    fn removed_drops_definition_and_progress() {
        let mut app = App::new();
        app.add_plugins(AdvancementsPlugin);
        let player = app.world_mut().spawn_empty().id();

        let mut progress: IndexMap<Identifier, AdvancementProgress> = IndexMap::new();
        let mut crit = AdvancementProgress::new();
        crit.insert(
            "x".to_owned(),
            CriterionProgress {
                date: Some(1),
            },
        );
        progress.insert(id("minecraft:tmp"), crit);

        apply_update_advancements(
            app.world_mut(),
            player,
            &ClientboundUpdateAdvancements {
                reset: false,
                added: vec![AdvancementHolder {
                    id: id("minecraft:tmp"),
                    value: make_adv(None, vec![vec!["x"]]),
                }],
                removed: vec![],
                progress,
                show_advancements: true,
            },
        );
        apply_update_advancements(
            app.world_mut(),
            player,
            &ClientboundUpdateAdvancements {
                reset: false,
                added: vec![],
                removed: vec![id("minecraft:tmp")],
                progress: IndexMap::new(),
                show_advancements: true,
            },
        );
        let advs = app.world().entity(player).get::<Advancements>().unwrap();
        assert!(advs.is_empty());
        assert!(advs.progress(&id("minecraft:tmp")).is_none());
    }
}
