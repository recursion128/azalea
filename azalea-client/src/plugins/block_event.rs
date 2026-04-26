//! `ClientboundBlockEvent` 包到 ECS 的接口。
//!
//! 协议层 `azalea_protocol::packets::game::c_block_event::ClientboundBlockEvent`
//! 是 vanilla 给 BlockEntity 驱动 client-side 视觉/听觉动作的通道：chest 开盖
//! 数（action_id=1, action_param=viewer_count）/ bell 钟体摆动（action_id=1,
//! action_param=direction）/ noteblock 弹音（action_id=note_pitch）/
//! piston 推动（action_id=extend|retract, action_param=facing）/ end_gateway 旋
//! 转 等。本插件把 packet 字段直接 fan-out 成 [`BlockEventReceived`] message，
//! 下游消费方（bbb-client 的 chest 盖子开合 / bell 钟体动画系统）按 `block` +
//! `action_id` 自行 dispatch。
//!
//! 协议解码层无任何补丁——`ClientboundBlockEvent` 已经在
//! `azalea-protocol/src/packets/game/c_block_event.rs` 全字段类型化（pos: BlockPos
//! / action_id: u8 / action_parameter: u8 / block: BlockKind）。

use azalea_core::position::BlockPos;
use azalea_protocol::packets::game::c_block_event::ClientboundBlockEvent;
use azalea_registry::builtin::BlockKind;
use azalea_world::WorldName;
use bevy_app::{App, Plugin};
use bevy_ecs::{prelude::*, system::SystemState};

pub struct BlockEventPlugin;

impl Plugin for BlockEventPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<BlockEventReceived>();
    }
}

/// 协议层 `ClientboundBlockEvent` 的 ECS 镜像。每条 packet 对应一条 message。
///
/// `world` 在 packet handler 里从 LocalEntity 的 `WorldName` 组件读出来，让
/// 下游 (world, pos) 复合 key 索引（与 [`BlockEntityRegistry`] 同模式）；
/// 没 `WorldName` 组件（join 前 / 已断）的 player 收到的 packet 直接 skip
/// 不发 message——pos 没有 world 上下文不可索引。
#[derive(Message, Clone, Debug)]
pub struct BlockEventReceived {
    pub world: WorldName,
    pub pos: BlockPos,
    pub action_id: u8,
    pub action_param: u8,
    pub block: BlockKind,
}

/// 单包 `ClientboundBlockEvent` 接入口——在协议 handler 里 `as_system::<...>` 调用。
pub fn apply_block_event(
    ecs: &mut World,
    player: Entity,
    packet: &ClientboundBlockEvent,
) {
    let mut state = SystemState::<(MessageWriter<BlockEventReceived>, Query<&WorldName>)>::new(ecs);
    let (mut writer, query) = state.get_mut(ecs);

    // 没 WorldName（join 前 / 已断）→ skip。pos 没有 world 上下文不可索引；
    // 与 BlockEntityPlugin `apply_block_entity_data` 同语义。
    let Ok(world) = query.get(player) else {
        return;
    };
    writer.write(BlockEventReceived {
        world: world.clone(),
        pos: packet.pos,
        action_id: packet.action_id,
        action_param: packet.action_parameter,
        block: packet.block,
    });

    state.apply(ecs);
}

#[cfg(test)]
mod tests {
    use bevy_app::App;
    use bevy_ecs::prelude::*;

    use super::*;

    fn drain_messages(app: &mut App) -> Vec<BlockEventReceived> {
        let world = app.world_mut();
        let mut events = world.resource_mut::<Messages<BlockEventReceived>>();
        let drained: Vec<_> = events.drain().collect();
        drained
    }

    #[test]
    fn apply_block_event_writes_message_with_world_name() {
        let mut app = App::new();
        app.add_plugins(BlockEventPlugin);
        let player = app
            .world_mut()
            .spawn(WorldName::new("minecraft:overworld"))
            .id();

        let packet = ClientboundBlockEvent {
            pos: BlockPos { x: 10, y: 64, z: -7 },
            action_id: 1,
            action_parameter: 2,
            block: BlockKind::Bell,
        };
        apply_block_event(app.world_mut(), player, &packet);

        let drained = drain_messages(&mut app);
        assert_eq!(drained.len(), 1);
        let ev = &drained[0];
        assert_eq!(ev.world, WorldName::new("minecraft:overworld"));
        assert_eq!(ev.pos, BlockPos { x: 10, y: 64, z: -7 });
        assert_eq!(ev.action_id, 1);
        assert_eq!(ev.action_param, 2);
        assert_eq!(ev.block, BlockKind::Bell);
    }

    #[test]
    fn apply_block_event_without_world_name_skipped() {
        let mut app = App::new();
        app.add_plugins(BlockEventPlugin);
        // player 实体没有 WorldName 组件——join 前 / 已断 azalea 客户端的状态。
        let player = app.world_mut().spawn_empty().id();

        let packet = ClientboundBlockEvent {
            pos: BlockPos { x: 0, y: 0, z: 0 },
            action_id: 0,
            action_parameter: 0,
            block: BlockKind::Chest,
        };
        apply_block_event(app.world_mut(), player, &packet);

        // 与 BlockEntityPlugin `apply_block_entity_data` 同语义：no-world skip。
        let drained = drain_messages(&mut app);
        assert!(drained.is_empty(), "no WorldName → no message");
    }

    #[test]
    fn apply_block_event_chest_action_param_carries_viewer_count() {
        // Chest open: action_id=1, action_parameter=viewer_count
        let mut app = App::new();
        app.add_plugins(BlockEventPlugin);
        let player = app.world_mut().spawn(WorldName::new("minecraft:overworld")).id();

        let packet = ClientboundBlockEvent {
            pos: BlockPos { x: 100, y: 64, z: 100 },
            action_id: 1,
            action_parameter: 3, // 3 viewers
            block: BlockKind::Chest,
        };
        apply_block_event(app.world_mut(), player, &packet);

        let drained = drain_messages(&mut app);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].action_id, 1);
        assert_eq!(drained[0].action_param, 3);
        assert_eq!(drained[0].block, BlockKind::Chest);
    }
}
