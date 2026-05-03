use azalea_client::test_utils::prelude::*;
use azalea_core::{
    delta::PositionDelta8,
    entity_id::MinecraftEntityId,
    position::{ChunkPos, Vec3},
};
use azalea_entity::{BodyYaw, LookDirection, indexing::EntityIdIndex};
use azalea_protocol::{
    common::movements::{PositionMoveRotation, RelativeMovements},
    packets::{
        ConnectionProtocol,
        game::{
            ClientboundAddEntity, ClientboundEntityPositionSync, ClientboundMoveEntityPosRot,
            ClientboundMoveEntityRot, ClientboundRotateHead, ClientboundTeleportEntity,
            c_move_entity_pos_rot::CompactLookDirection,
        },
    },
};
use azalea_registry::builtin::EntityKind;
use uuid::Uuid;

/// 验证 body_yaw 与 head_yaw（LookDirection.y_rot）拆分：
/// - AddEntity 同时初始化 BodyYaw（y_rot 字节）和 LookDirection（y_head_rot + x_rot）；
/// - RotateHead 只动 LookDirection.y_rot，不动 BodyYaw；
/// - MoveEntityRot / MoveEntityPosRot 只动 BodyYaw + LookDirection.x_rot，不动 LookDirection.y_rot；
/// - TeleportEntity 写 BodyYaw（y_rot），不动 LookDirection.y_rot。
#[test]
fn test_body_yaw_head_yaw_split() {
    let _lock = init();

    let mut s = Simulation::new(ConnectionProtocol::Game);
    s.receive_packet(default_login_packet());
    s.receive_packet(make_basic_empty_chunk(ChunkPos::new(0, 0), (384 + 64) / 16));
    s.tick();

    // spawn 一只 cow，body yaw 字节 = 64（约 90°），head yaw 字节 = -64（约 -90°），pitch 字节 = 32 (约 45°)
    s.receive_packet(ClientboundAddEntity {
        id: MinecraftEntityId(123),
        uuid: Uuid::from_u128(1234),
        entity_type: EntityKind::Cow,
        position: Vec3::new(0.5, 64., 0.5),
        movement: azalea_core::delta::LpVec3::Zero,
        x_rot: 32,
        y_rot: 64,
        y_head_rot: -64,
        data: 0,
    });
    s.tick();

    let cow = {
        let idx = s
            .app
            .world()
            .entity(s.entity)
            .get::<EntityIdIndex>()
            .unwrap();
        idx.get_by_minecraft_entity(MinecraftEntityId(123))
            .expect("cow should be indexed")
    };

    let look = *s.app.world().entity(cow).get::<LookDirection>().unwrap();
    let body = *s.app.world().entity(cow).get::<BodyYaw>().unwrap();
    assert!(
        (look.y_rot() - (-90.0)).abs() < 1e-3,
        "AddEntity 应把 y_head_rot 写到 LookDirection.y_rot, got {}",
        look.y_rot()
    );
    assert!(
        (look.x_rot() - 45.0).abs() < 1e-3,
        "AddEntity 应把 x_rot 写到 LookDirection.x_rot, got {}",
        look.x_rot()
    );
    assert!(
        (body.0 - 90.0).abs() < 1e-3,
        "AddEntity 应把 y_rot 写到 BodyYaw, got {}",
        body.0
    );

    // RotateHead 只改 head yaw（LookDirection.y_rot）
    s.receive_packet(ClientboundRotateHead {
        entity_id: MinecraftEntityId(123),
        y_head_rot: 32, // ~45°
    });
    s.tick();

    let look = *s.app.world().entity(cow).get::<LookDirection>().unwrap();
    let body = *s.app.world().entity(cow).get::<BodyYaw>().unwrap();
    assert!((look.y_rot() - 45.0).abs() < 1e-3, "head yaw 应被改到 45°");
    assert!(
        (look.x_rot() - 45.0).abs() < 1e-3,
        "RotateHead 不应改 pitch"
    );
    assert!(
        (body.0 - 90.0).abs() < 1e-3,
        "RotateHead 不应改 BodyYaw, got {}",
        body.0
    );

    // MoveEntityRot：写 body yaw + pitch
    s.receive_packet(ClientboundMoveEntityRot {
        entity_id: MinecraftEntityId(123),
        look_direction: CompactLookDirection {
            y_rot: -32, // body -45°
            x_rot: -16, // pitch ~-22.5°
        },
        on_ground: true,
    });
    s.tick();

    let look = *s.app.world().entity(cow).get::<LookDirection>().unwrap();
    let body = *s.app.world().entity(cow).get::<BodyYaw>().unwrap();
    assert!(
        (look.y_rot() - 45.0).abs() < 1e-3,
        "MoveEntityRot 不应动 LookDirection.y_rot, got {}",
        look.y_rot()
    );
    assert!(
        (look.x_rot() - (-22.5)).abs() < 1e-3,
        "MoveEntityRot 应改 pitch"
    );
    assert!(
        (body.0 - (-45.0)).abs() < 1e-3,
        "MoveEntityRot 应改 BodyYaw, got {}",
        body.0
    );

    // MoveEntityPosRot：同样只动 body + pitch（带 delta 不影响 yaw 拆分）
    s.receive_packet(ClientboundMoveEntityPosRot {
        entity_id: MinecraftEntityId(123),
        delta: PositionDelta8::default(),
        look_direction: CompactLookDirection {
            y_rot: 16, // body ~22.5°
            x_rot: 0,  // pitch 0
        },
        on_ground: true,
    });
    s.tick();

    let look = *s.app.world().entity(cow).get::<LookDirection>().unwrap();
    let body = *s.app.world().entity(cow).get::<BodyYaw>().unwrap();
    assert!(
        (look.y_rot() - 45.0).abs() < 1e-3,
        "MoveEntityPosRot 不应动 LookDirection.y_rot"
    );
    assert!(
        (look.x_rot() - 0.0).abs() < 1e-3,
        "MoveEntityPosRot 应把 pitch 改到 0"
    );
    assert!(
        (body.0 - 22.5).abs() < 1e-3,
        "MoveEntityPosRot 应改 BodyYaw, got {}",
        body.0
    );

    // TeleportEntity：vanilla 给 body yaw + pitch（不动 head yaw）
    s.receive_packet(ClientboundTeleportEntity {
        id: MinecraftEntityId(123),
        change: PositionMoveRotation {
            pos: Vec3::new(0.5, 64., 0.5),
            delta: Vec3::ZERO,
            look_direction: LookDirection::new(-90.0, 30.0),
        },
        relative: RelativeMovements::all_absolute(),
        on_ground: true,
    });
    s.tick();

    let look = *s.app.world().entity(cow).get::<LookDirection>().unwrap();
    let body = *s.app.world().entity(cow).get::<BodyYaw>().unwrap();
    assert!(
        (look.y_rot() - 45.0).abs() < 1e-3,
        "TeleportEntity 不应动 LookDirection.y_rot, got {}",
        look.y_rot()
    );
    assert!(
        (look.x_rot() - 30.0).abs() < 1e-3,
        "TeleportEntity 应把 pitch 写入 LookDirection.x_rot, got {}",
        look.x_rot()
    );
    assert!(
        (body.0 - (-90.0)).abs() < 1e-3,
        "TeleportEntity 应把 yaw 写入 BodyYaw, got {}",
        body.0
    );

    // EntityPositionSync：vanilla 同样给 body yaw + pitch（不动 head yaw）
    s.receive_packet(ClientboundEntityPositionSync {
        id: MinecraftEntityId(123),
        values: PositionMoveRotation {
            pos: Vec3::new(0.5, 64., 0.5),
            delta: Vec3::ZERO,
            look_direction: LookDirection::new(120.0, -30.0),
        },
        on_ground: true,
    });
    s.tick();

    let look = *s.app.world().entity(cow).get::<LookDirection>().unwrap();
    let body = *s.app.world().entity(cow).get::<BodyYaw>().unwrap();
    assert!(
        (look.y_rot() - 45.0).abs() < 1e-3,
        "EntityPositionSync 不应动 LookDirection.y_rot, got {}",
        look.y_rot()
    );
    assert!(
        (look.x_rot() - (-30.0)).abs() < 1e-3,
        "EntityPositionSync 应改 pitch, got {}",
        look.x_rot()
    );
    assert!(
        (body.0 - 120.0).abs() < 1e-3,
        "EntityPositionSync 应改 BodyYaw, got {}",
        body.0
    );
}
