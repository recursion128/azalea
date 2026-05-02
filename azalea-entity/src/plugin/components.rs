use azalea_block::fluid_state::FluidKind;
use azalea_core::{
    entity_id::MinecraftEntityId,
    position::{BlockPos, ChunkPos, Vec3},
};
use azalea_inventory::{ItemStack, components::EquipmentSlot};
use azalea_registry::builtin::EntityKind;
use azalea_world::WorldName;
use bevy_ecs::{bundle::Bundle, component::Component};
use derive_more::{Deref, DerefMut};
use uuid::Uuid;

use crate::{
    ActiveEffects, Attributes, EntityUuid, FluidOnEyes, LookDirection, Physics, Position,
    dimensions::EntityDimensions, indexing::EntityChunkPos,
};

/// A bundle of components that every entity has.
///
/// This doesn't contain metadata; that has to be added separately.
#[derive(Bundle)]
pub struct EntityBundle {
    pub kind: EntityKindComponent,
    pub uuid: EntityUuid,
    pub world_name: WorldName,
    pub position: Position,
    pub last_sent_position: LastSentPosition,

    pub chunk_pos: EntityChunkPos,

    pub physics: Physics,
    pub direction: LookDirection,
    pub dimensions: EntityDimensions,
    pub attributes: Attributes,
    pub jumping: Jumping,
    pub crouching: Crouching,
    pub fluid_on_eyes: FluidOnEyes,
    pub on_climbable: OnClimbable,
    pub active_effects: ActiveEffects,
}

impl EntityBundle {
    pub fn new(uuid: Uuid, pos: Vec3, kind: EntityKind, world_name: WorldName) -> Self {
        let dimensions = EntityDimensions::from(kind);

        Self {
            kind: EntityKindComponent(kind),
            uuid: EntityUuid(uuid),
            world_name,
            position: Position(pos),
            chunk_pos: EntityChunkPos(ChunkPos::from(&pos)),
            last_sent_position: LastSentPosition(pos),
            physics: Physics::new(&dimensions, pos),
            dimensions,
            direction: LookDirection::default(),

            attributes: Attributes::new(EntityKind::Player),

            jumping: Jumping(false),
            crouching: Crouching(false),
            fluid_on_eyes: FluidOnEyes(FluidKind::Empty),
            on_climbable: OnClimbable(false),
            active_effects: ActiveEffects::default(),
        }
    }
}

/// The equipment of a non-local entity, kept in sync from the
/// `ClientboundSetEquipment` packet.
///
/// Indexed by [`EquipmentSlot`]. Slots that the server never sends are kept as
/// [`ItemStack::Empty`]. The local player's equipment lives on
/// [`crate::inventory::Inventory`] instead and is **not** mirrored here.
#[derive(Component, Clone, Debug, Default)]
pub struct EntityEquipment {
    slots: [ItemStack; 8],
}

impl EntityEquipment {
    pub fn get(&self, slot: EquipmentSlot) -> &ItemStack {
        &self.slots[slot as usize]
    }

    pub fn set(&mut self, slot: EquipmentSlot, item: ItemStack) {
        self.slots[slot as usize] = item;
    }

    pub fn iter(&self) -> impl Iterator<Item = (EquipmentSlot, &ItemStack)> {
        EquipmentSlot::values()
            .into_iter()
            .map(move |slot| (slot, &self.slots[slot as usize]))
    }
}

/// The leash holder of a mob, kept in sync from the
/// `ClientboundSetEntityLink` packet.
///
/// `holder` is the [`MinecraftEntityId`] of whatever is currently holding the
/// lead — usually a player, but it can also be a fence's
/// `LeashFenceKnotEntity`. `None` means the mob is unleashed; vanilla
/// signals detach with either `dest_id == 0` (modern) or `dest_id == -1`
/// (older versions / fence-knot-removed path), and both are folded into
/// `None` at the handler.
///
/// The component remains attached after detach (with `holder = None`); the
/// presence of the component therefore means "we have ever received a
/// `SetEntityLink` for this entity", not "is currently leashed". Consumers
/// should match on `holder` rather than `With<Leashable>`.
///
/// We store the raw network entity id rather than an ECS [`Entity`] because
/// the holder may not have been added to [`crate::indexing::EntityIdIndex`]
/// yet at the time the link packet arrives (fence knot entities are spawned
/// in the same server tick but cross-packet ordering is not guaranteed).
/// Resolution to an ECS `Entity` is the consumer's responsibility — note
/// that the id is per-client, so in a swarm setup the same `MinecraftEntityId`
/// on different clients may map to different mobs; consumers that walk a
/// shared world should resolve via the originating client's `EntityIdIndex`.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Leashable {
    pub holder: Option<MinecraftEntityId>,
}

/// The owning entity of a projectile / fishing bobber, populated from the
/// `data` field of `ClientboundAddEntity`.
///
/// For fishing bobbers (`FishingBobber`) the owner is the player whose rod
/// cast the bobber — vanilla uses this id (not [`crate::metadata::HookedEntity`])
/// to draw the line between the rod tip and the bobber. `HookedEntity` is the
/// other side of the line: whatever the bobber has snagged (a fish item, a
/// mob, …) and is sent later via `ClientboundSetEntityData` metadata.
///
/// `None` means the spawn packet's `data` field was non-positive (`<= 0`),
/// which vanilla treats as "no owner" — typically when the projectile was
/// spawned by `/summon` or another world-side trigger rather than by an
/// entity. The component is only inserted for entity kinds whose
/// [object data](https://minecraft.wiki/w/Java_Edition_protocol/Object_data)
/// is documented as an owning entity id, currently:
/// `FishingBobber`, `Snowball`, `Egg`, `EnderPearl`, `ExperienceBottle`,
/// `SplashPotion`, `LingeringPotion`, `Arrow`, `SpectralArrow`, `Trident`.
/// Other projectile-shaped kinds (`ShulkerBullet`, `LlamaSpit`, …) overload
/// `data` differently and are intentionally not in the allowlist.
///
/// As with [`Leashable`] the value is stored as a raw [`MinecraftEntityId`]
/// (not an ECS [`Entity`]) because the owner may not yet be in the local
/// [`crate::indexing::EntityIdIndex`] at spawn time, and the same id may map
/// to different ECS entities across clients in a swarm.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProjectileOwner {
    pub owner: Option<MinecraftEntityId>,
}

/// Marker component for entities that are dead.
///
/// "Dead" means that the entity has 0 health.
#[derive(Clone, Component, Copy, Default)]
pub struct Dead;

/// A component NewType for [`EntityKind`].
///
/// Most of the time, you should be using `azalea_registry::EntityKind`
/// directly instead.
#[derive(Clone, Component, Copy, Debug, Deref, PartialEq)]
pub struct EntityKindComponent(pub EntityKind);

/// A marker component that signifies that this entity is "local" and shouldn't
/// be updated by other clients.
///
/// If this is for a client then all of our clients will have this.
///
/// This component is not removed from clients when they disconnect.
#[derive(Clone, Component, Copy, Debug, Default)]
pub struct LocalEntity;

impl FluidOnEyes {
    pub fn new(fluid: FluidKind) -> Self {
        Self(fluid)
    }
}

#[derive(Clone, Component, Copy, Debug, Deref, DerefMut, PartialEq)]
pub struct OnClimbable(bool);

/// A component that indicates whether the player is currently sneaking.
///
/// If the entity is a player but isn't a local player, then this is just a
/// shortcut for checking if the [`Pose`] is `Crouching`.
///
/// If you need to modify this value, use
/// `azalea_client::PhysicsState::trying_to_crouch` or `Client::set_crouching`
/// instead.
///
/// [`Pose`]: crate::data::Pose
#[derive(Clone, Component, Copy, Default, Deref, DerefMut)]
pub struct Crouching(bool);

/// A component that indicates whether the client has loaded.
///
/// This is updated by a system in `azalea-client`.
#[derive(Component)]
pub struct HasClientLoaded;

/// The second most recent position of the entity that was sent over the
/// network.
///
/// This is currently only updated for our own local player entities.
#[derive(Clone, Copy, Component, Debug, Default, Deref, DerefMut, PartialEq)]
pub struct LastSentPosition(Vec3);
impl From<&LastSentPosition> for Vec3 {
    fn from(value: &LastSentPosition) -> Self {
        value.0
    }
}
impl From<LastSentPosition> for ChunkPos {
    fn from(value: LastSentPosition) -> Self {
        ChunkPos::from(&value.0)
    }
}
impl From<LastSentPosition> for BlockPos {
    fn from(value: LastSentPosition) -> Self {
        BlockPos::from(&value.0)
    }
}
impl From<&LastSentPosition> for ChunkPos {
    fn from(value: &LastSentPosition) -> Self {
        ChunkPos::from(value.0)
    }
}
impl From<&LastSentPosition> for BlockPos {
    fn from(value: &LastSentPosition) -> Self {
        BlockPos::from(value.0)
    }
}

/// A component for entities that can jump.
///
/// If this is true, the entity will try to jump every tick. It's equivalent to
/// the space key being held in vanilla.
#[derive(Clone, Copy, Component, Debug, Default, Deref, DerefMut, Eq, PartialEq)]
pub struct Jumping(pub bool);
