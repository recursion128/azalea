//! `ClientboundOpenBook` / `ClientboundOpenSignEditor` 包到 ECS 的接口。
//!
//! 这两个 packet 都是 server **要求** client 弹出某个原生 GUI（书 / sign 编辑器）。
//! 本地 client 收到时 fold 进 per-player Component（[`OpenedBook`] /
//! [`OpenedSignEditor`]），UI 侧 query 到 `Some(_)` 就弹窗，弹完 / 用户取消
//! 后再写回 `None`。
//!
//! ## 协议层 packet 映射
//!
//! - `ClientboundOpenBook { hand }` → 写 [`OpenedBook`]，UI 拿 hand 决定
//!   读 main_hand / off_hand 的 written_book item NBT 渲染书页。
//! - `ClientboundOpenSignEditor { pos, is_front_text }` → 写
//!   [`OpenedSignEditor`]，UI 用 pos 索引到 SignData block entity 拿
//!   现有 4 行 text，编辑完发 `ServerboundSignUpdate`。
//!
//! ## 为什么是 Component 不是 Resource
//!
//! 与 boss_bar / scoreboard / advancements 同理：azalea swarm 一个 App
//! 跑多 client，"哪个 client 被要求开 book" 是每连接的事件，Resource
//! 全局会串台。

use azalea_core::position::BlockPos;
use azalea_protocol::packets::game::{
    c_open_book::ClientboundOpenBook, c_open_sign_editor::ClientboundOpenSignEditor,
    s_interact::InteractionHand,
};
use bevy_app::{App, Plugin};
use bevy_ecs::{prelude::*, system::SystemState};

pub struct ScreenOpenPlugin;

impl Plugin for ScreenOpenPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<OpenBookEvent>()
            .add_message::<OpenSignEditorEvent>();
    }
}

/// **Per-LocalEntity** 的当前书 GUI 状态。`Some(_)` 表示 server 刚要求
/// 打开当前手上的书。UI 弹完 / 用户关掉后写回 `None`。
#[derive(Component, Default, Debug, Clone, Copy)]
pub struct OpenedBook {
    pub hand: Option<InteractionHand>,
}

/// **Per-LocalEntity** 的当前 sign 编辑器状态。`Some(BlockPos)` 表示 server
/// 刚要求编辑该位置的 sign block entity；`is_front_text` 区分双面 sign
/// 的正反两面。
#[derive(Component, Default, Debug, Clone, Copy)]
pub struct OpenedSignEditor {
    pub target: Option<SignEditorTarget>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignEditorTarget {
    pub pos: BlockPos,
    pub is_front_text: bool,
}

/// `OpenedBook` 状态变化通知（每条 packet 一发）。`entity` 是收到 packet
/// 的 local player——swarm 多 client 时下游可以按 client 过滤。
#[derive(Message, Clone, Debug)]
pub struct OpenBookEvent {
    pub entity: Entity,
    pub hand: InteractionHand,
}

/// `OpenedSignEditor` 状态变化通知（每条 packet 一发）。
#[derive(Message, Clone, Debug)]
pub struct OpenSignEditorEvent {
    pub entity: Entity,
    pub pos: BlockPos,
    pub is_front_text: bool,
}

/// 单包 `ClientboundOpenBook` 接入口——在协议 handler 里
/// `as_system::<...>` 调用。
pub fn apply_open_book(ecs: &mut World, player: Entity, packet: &ClientboundOpenBook) {
    let mut state = SystemState::<(
        Commands,
        Query<&mut OpenedBook>,
        MessageWriter<OpenBookEvent>,
    )>::new(ecs);
    let (mut commands, mut query, mut writer) = state.get_mut(ecs);

    if let Ok(mut opened) = query.get_mut(player) {
        opened.hand = Some(packet.hand);
    } else {
        commands.entity(player).insert(OpenedBook {
            hand: Some(packet.hand),
        });
    }
    writer.write(OpenBookEvent {
        entity: player,
        hand: packet.hand,
    });
    state.apply(ecs);
}

/// 单包 `ClientboundOpenSignEditor` 接入口——在协议 handler 里
/// `as_system::<...>` 调用。
pub fn apply_open_sign_editor(
    ecs: &mut World,
    player: Entity,
    packet: &ClientboundOpenSignEditor,
) {
    let mut state = SystemState::<(
        Commands,
        Query<&mut OpenedSignEditor>,
        MessageWriter<OpenSignEditorEvent>,
    )>::new(ecs);
    let (mut commands, mut query, mut writer) = state.get_mut(ecs);

    let target = SignEditorTarget {
        pos: packet.pos,
        is_front_text: packet.is_front_text,
    };
    if let Ok(mut opened) = query.get_mut(player) {
        opened.target = Some(target);
    } else {
        commands.entity(player).insert(OpenedSignEditor {
            target: Some(target),
        });
    }
    writer.write(OpenSignEditorEvent {
        entity: player,
        pos: packet.pos,
        is_front_text: packet.is_front_text,
    });
    state.apply(ecs);
}

#[cfg(test)]
mod tests {
    use bevy_app::App;

    use super::*;

    #[test]
    fn open_book_sets_hand() {
        let mut app = App::new();
        app.add_plugins(ScreenOpenPlugin);
        let player = app.world_mut().spawn_empty().id();

        apply_open_book(
            app.world_mut(),
            player,
            &ClientboundOpenBook {
                hand: InteractionHand::OffHand,
            },
        );

        let opened = app.world().entity(player).get::<OpenedBook>().unwrap();
        assert_eq!(opened.hand, Some(InteractionHand::OffHand));
    }

    #[test]
    fn open_sign_editor_sets_target() {
        let mut app = App::new();
        app.add_plugins(ScreenOpenPlugin);
        let player = app.world_mut().spawn_empty().id();

        apply_open_sign_editor(
            app.world_mut(),
            player,
            &ClientboundOpenSignEditor {
                pos: BlockPos { x: 1, y: 64, z: -3 },
                is_front_text: false,
            },
        );
        let opened = app
            .world()
            .entity(player)
            .get::<OpenedSignEditor>()
            .unwrap();
        let target = opened.target.unwrap();
        assert_eq!(target.pos, BlockPos { x: 1, y: 64, z: -3 });
        assert!(!target.is_front_text);
    }

    #[test]
    fn two_local_players_isolated() {
        let mut app = App::new();
        app.add_plugins(ScreenOpenPlugin);
        let p1 = app.world_mut().spawn_empty().id();
        let p2 = app.world_mut().spawn_empty().id();

        apply_open_book(
            app.world_mut(),
            p1,
            &ClientboundOpenBook {
                hand: InteractionHand::MainHand,
            },
        );
        assert!(app.world().entity(p1).get::<OpenedBook>().is_some());
        assert!(app.world().entity(p2).get::<OpenedBook>().is_none());
    }
}
