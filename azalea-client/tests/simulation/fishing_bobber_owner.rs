use azalea_client::test_utils::prelude::*;
use azalea_core::{
    delta::LpVec3,
    entity_id::MinecraftEntityId,
    position::{ChunkPos, Vec3},
};
use azalea_entity::{ProjectileOwner, indexing::EntityIdIndex};
use azalea_protocol::packets::{ConnectionProtocol, game::ClientboundAddEntity};
use azalea_registry::builtin::EntityKind;
use uuid::Uuid;

fn projectile_spawn(
    kind: EntityKind,
    id: i32,
    owner_id: i32,
    pos: Vec3,
) -> ClientboundAddEntity {
    ClientboundAddEntity {
        id: id.into(),
        uuid: Uuid::from_u128(0xb0bbe5_u128.wrapping_mul((id as u32 as u128).wrapping_add(1))),
        entity_type: kind,
        position: pos,
        x_rot: 0,
        y_rot: 0,
        y_head_rot: 0,
        // ClientboundAddEntity.data carries the owning entity id for these
        // projectile kinds; vanilla uses it to draw the fishing line / to
        // attribute kills.
        data: owner_id,
        movement: LpVec3::Zero,
    }
}

fn fishing_bobber_spawn(id: i32, owner_id: i32, pos: Vec3) -> ClientboundAddEntity {
    projectile_spawn(EntityKind::FishingBobber, id, owner_id, pos)
}

/// Spawning a `FishingBobber` should attach a `ProjectileOwner` carrying the
/// entity id from the spawn packet's `data` field (vanilla overload of the
/// per-kind object data field).
#[test]
fn test_fishing_bobber_spawn_writes_projectile_owner() {
    let _lock = init();

    let mut s = Simulation::new(ConnectionProtocol::Game);
    s.receive_packet(default_login_packet());
    s.receive_packet(make_basic_empty_chunk(
        ChunkPos::new(0, 0),
        (384 + 64) / 16,
    ));
    s.tick();

    s.receive_packet(fishing_bobber_spawn(123, 42, Vec3::new(0.5, 64., 0.5)));
    s.tick();

    let bobber_entity = {
        let idx = s
            .app
            .world()
            .entity(s.entity)
            .get::<EntityIdIndex>()
            .expect("local player should have EntityIdIndex");
        idx.get_by_minecraft_entity(MinecraftEntityId(123))
            .expect("bobber should be indexed")
    };

    let owner = s
        .app
        .world()
        .entity(bobber_entity)
        .get::<ProjectileOwner>()
        .expect("FishingBobber spawn should insert ProjectileOwner");
    assert_eq!(owner.owner, Some(MinecraftEntityId(42)));
}

/// `data <= 0` (typical when the bobber is spawned via /summon rather than
/// thrown by a player) should resolve to `ProjectileOwner.owner = None`,
/// not panic and not propagate a bogus id like 0 / -1.
#[test]
fn test_fishing_bobber_spawn_with_no_owner() {
    let _lock = init();

    let mut s = Simulation::new(ConnectionProtocol::Game);
    s.receive_packet(default_login_packet());
    s.receive_packet(make_basic_empty_chunk(
        ChunkPos::new(0, 0),
        (384 + 64) / 16,
    ));
    s.tick();

    s.receive_packet(fishing_bobber_spawn(124, 0, Vec3::new(0.5, 64., 0.5)));
    s.receive_packet(fishing_bobber_spawn(125, -1, Vec3::new(0.5, 64., 0.5)));
    s.tick();

    for id in [124, 125] {
        let bobber_entity = {
            let idx = s
                .app
                .world()
                .entity(s.entity)
                .get::<EntityIdIndex>()
                .unwrap();
            idx.get_by_minecraft_entity(MinecraftEntityId(id)).unwrap()
        };
        let owner = s
            .app
            .world()
            .entity(bobber_entity)
            .get::<ProjectileOwner>()
            .unwrap();
        assert_eq!(
            owner.owner, None,
            "data {id} should resolve to no-owner",
        );
    }
}

/// Other projectile kinds in the allowlist (`Snowball`, `Egg`, `EnderPearl`,
/// `Arrow`, `Trident`, …) should also get a `ProjectileOwner`. We sample
/// `Snowball` and `Arrow` here as representatives — the handler uses a
/// single `matches!` allowlist so all of them share the same code path.
#[test]
fn test_other_projectiles_get_projectile_owner() {
    let _lock = init();

    let mut s = Simulation::new(ConnectionProtocol::Game);
    s.receive_packet(default_login_packet());
    s.receive_packet(make_basic_empty_chunk(
        ChunkPos::new(0, 0),
        (384 + 64) / 16,
    ));
    s.tick();

    s.receive_packet(projectile_spawn(
        EntityKind::Snowball,
        300,
        9,
        Vec3::new(0.5, 64., 0.5),
    ));
    s.receive_packet(projectile_spawn(
        EntityKind::Arrow,
        301,
        10,
        Vec3::new(0.5, 64., 0.5),
    ));
    s.tick();

    for (id, expected_owner) in [(300, 9), (301, 10)] {
        let entity = {
            let idx = s
                .app
                .world()
                .entity(s.entity)
                .get::<EntityIdIndex>()
                .unwrap();
            idx.get_by_minecraft_entity(MinecraftEntityId(id)).unwrap()
        };
        let owner = s
            .app
            .world()
            .entity(entity)
            .get::<ProjectileOwner>()
            .unwrap_or_else(|| panic!("entity id {id} should have ProjectileOwner"));
        assert_eq!(owner.owner, Some(MinecraftEntityId(expected_owner)));
    }
}

/// Non-projectile spawns (e.g. `Cow`) should NOT get a `ProjectileOwner`
/// — that field is reserved for entity kinds whose `AddEntity.data` is
/// documented as an owning entity id.
#[test]
fn test_non_projectile_spawn_skips_projectile_owner() {
    let _lock = init();

    let mut s = Simulation::new(ConnectionProtocol::Game);
    s.receive_packet(default_login_packet());
    s.receive_packet(make_basic_empty_chunk(
        ChunkPos::new(0, 0),
        (384 + 64) / 16,
    ));
    s.tick();

    s.receive_packet(make_basic_add_entity(EntityKind::Cow, 200, (0.5, 64., 0.5)));
    s.tick();

    let cow_entity = {
        let idx = s
            .app
            .world()
            .entity(s.entity)
            .get::<EntityIdIndex>()
            .unwrap();
        idx.get_by_minecraft_entity(MinecraftEntityId(200)).unwrap()
    };
    assert!(
        s.app
            .world()
            .entity(cow_entity)
            .get::<ProjectileOwner>()
            .is_none(),
    );
}
