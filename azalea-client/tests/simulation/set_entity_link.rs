use azalea_client::test_utils::prelude::*;
use azalea_core::{entity_id::MinecraftEntityId, position::ChunkPos};
use azalea_entity::{Leashable, indexing::EntityIdIndex};
use azalea_protocol::packets::{ConnectionProtocol, game::ClientboundSetEntityLink};
use azalea_registry::builtin::EntityKind;

/// `ClientboundSetEntityLink` should write a `Leashable` component onto the
/// source entity carrying the holder's network entity id; sending a follow-up
/// packet with `dest_id = 0` should clear it back to `None` (vanilla's
/// detach-the-lead path).
#[test]
fn test_set_entity_link_attach_then_detach() {
    let _lock = init();

    let mut s = Simulation::new(ConnectionProtocol::Game);
    s.receive_packet(default_login_packet());
    s.receive_packet(make_basic_empty_chunk(
        ChunkPos::new(0, 0),
        (384 + 64) / 16,
    ));
    s.tick();

    // spawn a cow we can leash
    s.receive_packet(make_basic_add_entity(EntityKind::Cow, 123, (0.5, 64., 0.5)));
    s.tick();

    let cow_entity = {
        let idx = s
            .app
            .world()
            .entity(s.entity)
            .get::<EntityIdIndex>()
            .expect("local player should have EntityIdIndex");
        idx.get_by_minecraft_entity(MinecraftEntityId(123))
            .expect("cow should be indexed")
    };

    // before the link packet there's no Leashable
    assert!(
        s.app
            .world()
            .entity(cow_entity)
            .get::<Leashable>()
            .is_none(),
    );

    // attach: dest_id 1 (the local player); handler should write
    // Leashable.holder = Some(1).
    s.receive_packet(ClientboundSetEntityLink {
        source_id: MinecraftEntityId(123),
        dest_id: MinecraftEntityId(1),
    });
    s.tick();

    let leashable = s
        .app
        .world()
        .entity(cow_entity)
        .get::<Leashable>()
        .expect("Leashable should be inserted after link");
    assert_eq!(leashable.holder, Some(MinecraftEntityId(1)));

    // detach: dest_id 0 -> Leashable.holder back to None.
    s.receive_packet(ClientboundSetEntityLink {
        source_id: MinecraftEntityId(123),
        dest_id: MinecraftEntityId(0),
    });
    s.tick();

    let leashable = s
        .app
        .world()
        .entity(cow_entity)
        .get::<Leashable>()
        .expect("Leashable should still exist after detach");
    assert_eq!(leashable.holder, None);

    // re-attach to a different holder, then detach via -1 (older vanilla
    // versions / fence-knot-removed path use a negative dest_id rather than 0).
    s.receive_packet(ClientboundSetEntityLink {
        source_id: MinecraftEntityId(123),
        dest_id: MinecraftEntityId(7),
    });
    s.tick();
    let leashable = s
        .app
        .world()
        .entity(cow_entity)
        .get::<Leashable>()
        .unwrap();
    assert_eq!(leashable.holder, Some(MinecraftEntityId(7)));

    s.receive_packet(ClientboundSetEntityLink {
        source_id: MinecraftEntityId(123),
        dest_id: MinecraftEntityId(-1),
    });
    s.tick();
    let leashable = s
        .app
        .world()
        .entity(cow_entity)
        .get::<Leashable>()
        .unwrap();
    assert_eq!(
        leashable.holder, None,
        "negative dest_id should also be treated as detach",
    );
}

/// Unknown source entity ids should be ignored without panicking. Vanilla can
/// send a `SetEntityLink` for an entity we never received a spawn packet for
/// (e.g. spawn was filtered by chunk distance) and it shouldn't crash the
/// client.
#[test]
fn test_set_entity_link_unknown_source_is_noop() {
    let _lock = init();

    let mut s = Simulation::new(ConnectionProtocol::Game);
    s.receive_packet(default_login_packet());
    s.tick();

    s.receive_packet(ClientboundSetEntityLink {
        source_id: MinecraftEntityId(9999),
        dest_id: MinecraftEntityId(1),
    });
    s.tick();
}
