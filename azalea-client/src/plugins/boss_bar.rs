//! `ClientboundBossEvent` 包到 ECS 的接口。
//!
//! 协议层 `azalea_protocol::packets::game::c_boss_event::ClientboundBossEvent`
//! 用一个 UUID + Operation 状态机维护多个 boss bar：
//! `Add` 注册（name / progress / style / properties 全字段）、`Remove` 销毁、
//! 其余 4 个 update_* 增量改字段。本插件把 packet 的状态机直接 fold 进
//! [`BossBars`] **per-player Component**（key = UUID），下游 UI（hud boss bar
//! 渲染）拉 local player 的 `&BossBars` 直接 enumerate。
//!
//! ## 为什么是 Component 不是 Resource
//!
//! azalea swarm 一个 App 跑多 client，boss bar 是**每连接私有**——A client
//! 收到的 dragon boss bar 不应渲染到 B client。Resource 全局共享会串台，
//! 走 `Component-on-LocalEntity` 与 [`Inventory`] 同语义。
//!
//! 协议解码层无任何补丁——所有 enum 已经在
//! `azalea-protocol/src/packets/game/c_boss_event.rs` 全字段类型化。

use std::collections::HashMap;

use azalea_chat::FormattedText;
use azalea_protocol::packets::game::c_boss_event::{
    BossBarColor, BossBarOverlay, ClientboundBossEvent, Operation,
};
use bevy_app::{App, Plugin};
use bevy_ecs::{prelude::*, system::SystemState};
use uuid::Uuid;

pub struct BossBarPlugin;

impl Plugin for BossBarPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<BossBarUpdated>();
    }
}

/// 单个 boss bar 的快照。字段语义直接对应 vanilla wire format，留给 UI
/// 侧自己决定如何渲染（颜色映射 / 进度条样式 / dark sky / fog post-process /
/// boss music 触发）。
#[derive(Clone, Debug, PartialEq)]
pub struct BossBar {
    pub name: FormattedText,
    pub progress: f32,
    pub color: BossBarColor,
    pub overlay: BossBarOverlay,
    /// vanilla `darken_screen`：darken sky + fade hotbar。
    pub dark_sky: bool,
    /// vanilla `play_music`：触发 boss music（dragon / wither）。
    pub music: bool,
    /// vanilla `create_world_fog`：strong fog post-process。
    pub fog: bool,
}

/// **Per-LocalEntity** 的 boss bar 索引。`map` 由 boss UUID（vanilla 给的稳定
/// id）索引——同一时刻可同时存在多个 boss（dragon + wither）。
#[derive(Component, Default, Debug, Clone)]
pub struct BossBars {
    map: HashMap<Uuid, BossBar>,
}

impl BossBars {
    pub fn get(&self, id: &Uuid) -> Option<&BossBar> {
        self.map.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Uuid, &BossBar)> {
        self.map.iter()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// 单个 boss bar 状态变化的通知：每条 packet 对应一条 message（含 Remove）。
/// 下游 UI 想懒挂 / 懒销毁 effect 监听这个就够。`entity` 是收到 packet 的
/// local player——swarm 多 client 时下游可以按 client 过滤。
#[derive(Message, Clone, Debug)]
pub struct BossBarUpdated {
    pub entity: Entity,
    pub id: Uuid,
    pub change: BossBarChange,
}

#[derive(Clone, Debug)]
pub enum BossBarChange {
    Added,
    Removed,
    Updated,
}

/// 单包 `ClientboundBossEvent` 接入口——在协议 handler 里 `as_system::<...>` 调用。
pub fn apply_boss_event(ecs: &mut World, player: Entity, packet: &ClientboundBossEvent) {
    let mut state = SystemState::<(
        Commands,
        Query<&mut BossBars>,
        MessageWriter<BossBarUpdated>,
    )>::new(ecs);
    let (mut commands, mut query, mut writer) = state.get_mut(ecs);

    let id = packet.id;
    // BossBars 不在 JoinedClientBundle 里（避免每 client 多挂一个空 HashMap），
    // lazily 在第一条 packet 时插入。
    let change = if let Ok(mut bars) = query.get_mut(player) {
        fold_packet(&mut bars, packet)
    } else {
        let mut new_bars = BossBars::default();
        let change = fold_packet(&mut new_bars, packet);
        commands.entity(player).insert(new_bars);
        change
    };
    writer.write(BossBarUpdated {
        entity: player,
        id,
        change,
    });
    state.apply(ecs);
}

fn fold_packet(bars: &mut BossBars, packet: &ClientboundBossEvent) -> BossBarChange {
    let id = packet.id;
    match &packet.operation {
        Operation::Add(add) => {
            bars.map.insert(
                id,
                BossBar {
                    name: add.name.clone(),
                    progress: add.progress,
                    color: add.style.color,
                    overlay: add.style.overlay,
                    dark_sky: add.properties.darken_screen,
                    music: add.properties.play_music,
                    fog: add.properties.create_world_fog,
                },
            );
            BossBarChange::Added
        }
        Operation::Remove => {
            bars.map.remove(&id);
            BossBarChange::Removed
        }
        Operation::UpdateProgress(p) => {
            if let Some(bar) = bars.map.get_mut(&id) {
                bar.progress = *p;
            }
            BossBarChange::Updated
        }
        Operation::UpdateName(n) => {
            if let Some(bar) = bars.map.get_mut(&id) {
                bar.name = n.clone();
            }
            BossBarChange::Updated
        }
        Operation::UpdateStyle(s) => {
            if let Some(bar) = bars.map.get_mut(&id) {
                bar.color = s.color;
                bar.overlay = s.overlay;
            }
            BossBarChange::Updated
        }
        Operation::UpdateProperties(props) => {
            if let Some(bar) = bars.map.get_mut(&id) {
                bar.dark_sky = props.darken_screen;
                bar.music = props.play_music;
                bar.fog = props.create_world_fog;
            }
            BossBarChange::Updated
        }
    }
}

#[cfg(test)]
mod tests {
    use azalea_protocol::packets::game::c_boss_event::{AddOperation, Properties, Style};
    use bevy_app::App;

    use super::*;

    fn id1() -> Uuid {
        Uuid::from_u128(0x0000_0001_0000_0000_0000_0000_0000_0001)
    }

    #[test]
    fn add_then_update_progress_updates_state() {
        let mut app = App::new();
        app.add_plugins(BossBarPlugin);
        let player = app.world_mut().spawn_empty().id();

        apply_boss_event(
            app.world_mut(),
            player,
            &ClientboundBossEvent {
                id: id1(),
                operation: Operation::Add(AddOperation {
                    name: FormattedText::from("Ender Dragon".to_owned()),
                    progress: 1.0,
                    style: Style {
                        color: BossBarColor::Pink,
                        overlay: BossBarOverlay::Progress,
                    },
                    properties: Properties {
                        darken_screen: true,
                        play_music: true,
                        create_world_fog: false,
                    },
                }),
            },
        );

        let bars = app.world().entity(player).get::<BossBars>().unwrap();
        assert_eq!(bars.len(), 1);
        let bar = bars.get(&id1()).unwrap();
        assert_eq!(bar.progress, 1.0);
        assert_eq!(bar.color, BossBarColor::Pink);
        assert!(bar.dark_sky);
        assert!(bar.music);
        assert!(!bar.fog);

        apply_boss_event(
            app.world_mut(),
            player,
            &ClientboundBossEvent {
                id: id1(),
                operation: Operation::UpdateProgress(0.5),
            },
        );
        let bars = app.world().entity(player).get::<BossBars>().unwrap();
        assert_eq!(bars.get(&id1()).unwrap().progress, 0.5);
    }

    #[test]
    fn remove_drops_entry() {
        let mut app = App::new();
        app.add_plugins(BossBarPlugin);
        let player = app.world_mut().spawn_empty().id();

        apply_boss_event(
            app.world_mut(),
            player,
            &ClientboundBossEvent {
                id: id1(),
                operation: Operation::Add(AddOperation {
                    name: FormattedText::from("Wither".to_owned()),
                    progress: 1.0,
                    style: Style {
                        color: BossBarColor::Purple,
                        overlay: BossBarOverlay::Notched6,
                    },
                    properties: Properties {
                        darken_screen: false,
                        play_music: false,
                        create_world_fog: false,
                    },
                }),
            },
        );
        assert_eq!(
            app.world().entity(player).get::<BossBars>().unwrap().len(),
            1
        );

        apply_boss_event(
            app.world_mut(),
            player,
            &ClientboundBossEvent {
                id: id1(),
                operation: Operation::Remove,
            },
        );
        assert!(
            app.world()
                .entity(player)
                .get::<BossBars>()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn two_local_players_isolated() {
        // azalea swarm: two LocalEntity, each receives its own boss event;
        // state must not bleed between them.
        let mut app = App::new();
        app.add_plugins(BossBarPlugin);
        let p1 = app.world_mut().spawn_empty().id();
        let p2 = app.world_mut().spawn_empty().id();

        apply_boss_event(
            app.world_mut(),
            p1,
            &ClientboundBossEvent {
                id: id1(),
                operation: Operation::Add(AddOperation {
                    name: FormattedText::from("only on p1".to_owned()),
                    progress: 0.7,
                    style: Style {
                        color: BossBarColor::Red,
                        overlay: BossBarOverlay::Progress,
                    },
                    properties: Properties {
                        darken_screen: false,
                        play_music: false,
                        create_world_fog: false,
                    },
                }),
            },
        );
        assert_eq!(
            app.world().entity(p1).get::<BossBars>().unwrap().len(),
            1
        );
        assert!(app.world().entity(p2).get::<BossBars>().is_none());
    }
}
