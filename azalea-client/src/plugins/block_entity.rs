//! 协议 `BlockEntity` / `ClientboundBlockEntityData` 解码到 ECS 的接口。
//!
//! 协议层 `azalea_protocol::packets::game::c_level_chunk_with_light::BlockEntity`
//! 持的是 raw `simdnbt::owned::Nbt`；本插件订阅 chunk 收包 + 单包 block-entity
//! 更新事件，把 `data` 走 [`BlockEntityData::from_nbt`](azalea_inventory::block_entity_data::BlockEntityData::from_nbt)
//! 解成类型化字段后写进 [`BlockEntityRegistry`] resource，并发 [`BlockEntityUpdated`]
//! 消息让其他系统按需订阅。
//!
//! ## 索引
//!
//! 一个 azalea 进程能有多个 swarm-client 共享同一份世界，registry 用 `(WorldName,
//! BlockPos)` 作为复合 key。同一 world 不同 client 都看见同一份记录，避免重复
//! 解码 / 重复存。
//!
//! ## 生命周期
//!
//! - **写入**：chunk 整片收到（`ReceiveChunkEvent`）时把 `chunk_data.block_entities`
//!   全量解码 + 写入；单包 `ClientboundBlockEntityData` 在协议 handler 里直接调用
//!   [`BlockEntityRegistry::insert`] + 发 [`BlockEntityUpdated`]。
//! - **失效**：当前**不**主动删条目（chunk unload 通路下游 azalea 没暴露统一事件
//!   入口）。下游真要做 unload 清理可以监听 chunk 替换：每次新 `ReceiveChunkEvent`
//!   推到本系统时会先按 `ChunkPos` 删掉所有旧条目再写入，等价于 chunk 替换。
//!   长会话的 stale-entry 内存占用 ~MB 级，留作 follow-up。

use std::collections::HashMap;

use azalea_core::position::{BlockPos, ChunkPos};
// 给下游一个 stable 的 typed enum 入口：`azalea::block_entity::BlockEntityData`
// 等同 `azalea_inventory::block_entity_data::BlockEntityData`，ECS-side 消费方
// `use azalea::block_entity::*` 就能拿到 plugin / resource / event / typed
// enum 一整套，不必再单独 import azalea_inventory 子模块。
pub use azalea_inventory::block_entity_data::{
    BannerData, BannerPatternLayer, BellData, BlockEntityData, ChestData, ChestItem, SignData,
    SignFace, SkullData,
};
use azalea_protocol::packets::game::{
    c_block_entity_data::ClientboundBlockEntityData,
    c_level_chunk_with_light::BlockEntity as ProtoBlockEntity,
};
use azalea_world::WorldName;
use bevy_app::{App, Plugin, Update};
use bevy_ecs::{prelude::*, system::SystemState};
use tracing::trace;

use crate::{chunks::ReceiveChunkEvent, local_player::WorldHolder};

pub struct BlockEntityPlugin;

impl Plugin for BlockEntityPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BlockEntityRegistry>()
            .add_message::<BlockEntityUpdated>()
            .add_systems(
                Update,
                handle_chunk_block_entities
                    .after(crate::chunks::handle_receive_chunk_event),
            );
    }
}

/// 全局 BlockEntity 索引。`map` 由 `(WorldName, BlockPos)` 复合 key 索引——同一
/// azalea 进程多 swarm-client 共享世界时，同一 world 不同 client 看见同一份。
///
/// 下游 query：拿到 `Res<BlockEntityRegistry>` + `WorldHolder.shared` 解出
/// `WorldName`，然后 `registry.get(&world, pos)`。
#[derive(Resource, Default, Debug)]
pub struct BlockEntityRegistry {
    map: HashMap<(WorldName, BlockPos), BlockEntityData>,
}

impl BlockEntityRegistry {
    pub fn get(&self, world: &WorldName, pos: BlockPos) -> Option<&BlockEntityData> {
        self.map.get(&(world.clone(), pos))
    }

    pub fn insert(&mut self, world: WorldName, pos: BlockPos, data: BlockEntityData) {
        self.map.insert((world, pos), data);
    }

    /// 整片 chunk 替换：移除该 (world, chunk_pos) 内所有旧条目。供 chunk 收到
    /// 新 packet 时清掉脏数据用——避免上一片 chunk 内残留的 BlockEntity 在本片
    /// 已被破坏 / 移动后还留在 registry 里。
    pub fn drop_chunk(&mut self, world: &WorldName, chunk_pos: ChunkPos) {
        self.map.retain(|(w, pos), _| {
            !(w == world
                && (pos.x.div_euclid(16) == chunk_pos.x)
                && (pos.z.div_euclid(16) == chunk_pos.z))
        });
    }

    pub fn iter(&self) -> impl Iterator<Item = (&(WorldName, BlockPos), &BlockEntityData)> {
        self.map.iter()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// 单条 BlockEntity 更新通知：写 registry 后发出，供下游做"该位置 BE 变了"
/// 的反应（重 mesh、收容器 GUI 状态、render dirty 等）。
#[derive(Message, Clone, Debug)]
pub struct BlockEntityUpdated {
    pub world: WorldName,
    pub pos: BlockPos,
    pub data: BlockEntityData,
}

/// `ReceiveChunkEvent` 进来后，把 packet 的 `chunk_data.block_entities` 全量
/// 解码后写进 registry。`packed_xz`（4-bit nibble 对）+ `y`（i16 包成 u16）
/// 解出 chunk-relative 偏移再加 chunk origin。
///
/// chunk 替换语义：每片 chunk 收到时先 [`BlockEntityRegistry::drop_chunk`]
/// 清掉旧 entries 再写新值。
pub fn handle_chunk_block_entities(
    mut events: MessageReader<ReceiveChunkEvent>,
    mut registry: ResMut<BlockEntityRegistry>,
    mut writer: MessageWriter<BlockEntityUpdated>,
    query: Query<(&WorldHolder, Option<&WorldName>)>,
) {
    for event in events.read() {
        let Ok((_holder, Some(world_name))) = query.get(event.entity) else {
            continue;
        };
        let chunk_pos = ChunkPos::new(event.packet.x, event.packet.z);
        registry.drop_chunk(world_name, chunk_pos);

        for be in &event.packet.chunk_data.block_entities {
            let pos = decode_block_entity_pos(chunk_pos, be);
            let data = BlockEntityData::from_nbt(be.kind, &be.data);
            trace!(
                "block entity at {pos:?} kind={:?} → {:?}",
                be.kind,
                data.kind()
            );
            registry.insert(world_name.clone(), pos, data.clone());
            writer.write(BlockEntityUpdated {
                world: world_name.clone(),
                pos,
                data,
            });
        }
    }
}

/// 单包 `ClientboundBlockEntityData`（`BlockEntityData` 包，不带 chunk）的
/// 接入口——在协议 handler 里 `as_system::<...>` 调用。
pub fn apply_block_entity_data(
    ecs: &mut World,
    player: Entity,
    packet: &ClientboundBlockEntityData,
) {
    let mut state = SystemState::<(
        ResMut<BlockEntityRegistry>,
        MessageWriter<BlockEntityUpdated>,
        Query<&WorldName>,
    )>::new(ecs);
    let (mut registry, mut writer, query) = state.get_mut(ecs);

    let Ok(world_name) = query.get(player) else {
        return;
    };

    let data = BlockEntityData::from_nbt(packet.block_entity_type, &packet.tag);
    registry.insert(world_name.clone(), packet.pos, data.clone());
    writer.write(BlockEntityUpdated {
        world: world_name.clone(),
        pos: packet.pos,
        data,
    });

    state.apply(ecs);
}

fn decode_block_entity_pos(chunk_pos: ChunkPos, be: &ProtoBlockEntity) -> BlockPos {
    // Wire packing：`packed_xz = ((blockX & 15) << 4) | (blockZ & 15)`，
    // `y` 是 i16 在 wire 上 widen 到 u16，需要 sign-extend 回 i32（高位 chunk
    // -64 时 y 也可能是负值）。
    let local_x = ((be.packed_xz >> 4) & 0x0F) as i32;
    let local_z = (be.packed_xz & 0x0F) as i32;
    BlockPos {
        x: chunk_pos.x * 16 + local_x,
        y: be.y as i16 as i32,
        z: chunk_pos.z * 16 + local_z,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pos_decoded_from_chunk_relative_packed_xz() {
        // packed_xz = (5 << 4) | 11 = 0x5B；y = 64
        let be = ProtoBlockEntity {
            packed_xz: 0x5B,
            y: 64,
            kind: azalea_registry::builtin::BlockEntityKind::Chest,
            data: simdnbt::owned::Nbt::None,
        };
        let pos = decode_block_entity_pos(ChunkPos::new(2, -3), &be);
        assert_eq!(pos.x, 2 * 16 + 5);
        assert_eq!(pos.z, -3 * 16 + 11);
        assert_eq!(pos.y, 64);
    }

    #[test]
    fn pos_decoded_with_negative_y_via_sign_extend() {
        // Y = -64 → on wire as i16 = -64 → u16 = 0xFFC0
        let be = ProtoBlockEntity {
            packed_xz: 0x00,
            y: 0xFFC0u16,
            kind: azalea_registry::builtin::BlockEntityKind::Chest,
            data: simdnbt::owned::Nbt::None,
        };
        let pos = decode_block_entity_pos(ChunkPos::new(0, 0), &be);
        assert_eq!(pos.y, -64);
    }

    #[test]
    fn registry_drop_chunk_removes_only_that_chunk() {
        let mut reg = BlockEntityRegistry::default();
        let world = WorldName::new("minecraft:overworld");
        reg.insert(
            world.clone(),
            BlockPos { x: 0, y: 64, z: 0 },
            BlockEntityData::Unknown {
                kind: azalea_registry::builtin::BlockEntityKind::Chest,
            },
        );
        reg.insert(
            world.clone(),
            BlockPos { x: 32, y: 64, z: 0 },
            BlockEntityData::Unknown {
                kind: azalea_registry::builtin::BlockEntityKind::Sign,
            },
        );
        assert_eq!(reg.len(), 2);
        reg.drop_chunk(&world, ChunkPos::new(0, 0));
        assert_eq!(reg.len(), 1);
        assert!(
            reg.get(&world, BlockPos { x: 32, y: 64, z: 0 })
                .is_some()
        );
    }
}
