use azalea_buf::AzBuf;
use azalea_core::{delta::LpVec3, entity_id::MinecraftEntityId, position::Vec3};
use azalea_protocol_macros::ClientboundGamePacket;
use azalea_registry::builtin::EntityKind;
#[cfg(feature = "bevy_ecs")]
use azalea_world::WorldName;
use uuid::Uuid;

#[derive(AzBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundAddEntity {
    /// The numeric ID of the entity being added to the world.
    #[var]
    pub id: MinecraftEntityId,
    pub uuid: Uuid,
    pub entity_type: EntityKind,
    pub position: Vec3,
    pub movement: LpVec3,
    pub x_rot: i8,
    pub y_rot: i8,
    pub y_head_rot: i8,
    /// The entity's "object data". This is unused for most entities.
    ///
    /// Projectiles and fishing hooks treat this like a [`MinecraftEntityId`].
    /// Falling blocks treat it as a [`BlockState`](azalea_block::BlockState).
    /// Other entities may treat it as another enum variant.
    ///
    /// See [the wiki](https://minecraft.wiki/w/Java_Edition_protocol/Object_data)
    /// for more information about this field.
    #[var]
    pub data: i32,
}

impl ClientboundAddEntity {
    /// Make the entity into a bundle that can be inserted into the ECS.
    ///
    /// 三个朝向字节分别落到不同组件：head yaw + pitch 写 [`azalea_entity::LookDirection`]
    /// （head 方向也是 player 视角方向），body yaw 写 [`azalea_entity::BodyYaw`]。
    ///
    /// You must apply the metadata after inserting the bundle with
    /// [`Self::apply_metadata`].
    #[cfg(feature = "bevy_ecs")]
    pub fn as_entity_bundle(&self, world_name: WorldName) -> azalea_entity::EntityBundle {
        let look = azalea_entity::LookDirection::new(
            (self.y_head_rot as i32 * 360) as f32 / 256.,
            (self.x_rot as i32 * 360) as f32 / 256.,
        );
        let body_yaw = (self.y_rot as i32 * 360) as f32 / 256.;
        let mut bundle =
            azalea_entity::EntityBundle::new(self.uuid, self.position, self.entity_type, world_name);
        bundle.direction = look;
        bundle.body_yaw = azalea_entity::BodyYaw(body_yaw);
        bundle
    }

    /// Apply the default metadata for the given entity.
    #[cfg(feature = "bevy_ecs")]
    pub fn apply_metadata(&self, entity: &mut bevy_ecs::system::EntityCommands) {
        azalea_entity::metadata::apply_default_metadata(entity, self.entity_type);
    }
}
