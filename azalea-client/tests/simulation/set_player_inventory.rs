use azalea_chat::FormattedText;
use azalea_client::test_utils::prelude::*;
use azalea_entity::inventory::Inventory;
use azalea_inventory::ItemStack;
use azalea_protocol::packets::{
    ConnectionProtocol,
    game::{ClientboundContainerClose, ClientboundOpenScreen, ClientboundSetPlayerInventory},
};
use azalea_registry::builtin::{ItemKind, MenuKind};

/// MC 26.1's vanilla server pushes single-slot updates to the player
/// inventory via `ClientboundSetPlayerInventory` (the result of e.g.
/// `/give @bot stone`). Earlier protocol versions used
/// `ClientboundContainerSetSlot { container_id: 0, slot, ... }`. Make
/// sure the handler actually writes into the player inventory menu and,
/// when a container is open, mirrors hotbar / storage updates into the
/// container menu's player slots — vanilla aliases those slots to the
/// same `ItemStack` instances.
#[test]
fn test_set_player_inventory() {
    let _lock = init();

    let mut s = Simulation::new(ConnectionProtocol::Game);
    s.receive_packet(default_login_packet());
    s.tick();

    // Hotbar slot 0 (vanilla index) -> menu protocol index 36, mirrored
    // into `Inventory::held_item()` since selected_hotbar_slot defaults
    // to 0.
    s.receive_packet(ClientboundSetPlayerInventory {
        slot: 0,
        contents: ItemStack::new(ItemKind::Stone, 1),
    });
    s.tick();
    s.with_component(|inv: &Inventory| {
        assert_eq!(
            inv.inventory_menu.slot(36),
            Some(&ItemStack::new(ItemKind::Stone, 1)),
        );
        assert_eq!(inv.held_item(), &ItemStack::new(ItemKind::Stone, 1));
    });

    // Storage slot 9 -> menu protocol index 9.
    s.receive_packet(ClientboundSetPlayerInventory {
        slot: 9,
        contents: ItemStack::new(ItemKind::DiamondPickaxe, 1),
    });
    s.tick();
    s.with_component(|inv: &Inventory| {
        assert_eq!(
            inv.inventory_menu.slot(9),
            Some(&ItemStack::new(ItemKind::DiamondPickaxe, 1)),
        );
    });

    // Armor slot 39 (head) -> menu protocol index 5.
    s.receive_packet(ClientboundSetPlayerInventory {
        slot: 39,
        contents: ItemStack::new(ItemKind::IronHelmet, 1),
    });
    s.tick();
    s.with_component(|inv: &Inventory| {
        assert_eq!(
            inv.inventory_menu.slot(5),
            Some(&ItemStack::new(ItemKind::IronHelmet, 1)),
        );
    });

    // Offhand slot 40 -> menu protocol index 45.
    s.receive_packet(ClientboundSetPlayerInventory {
        slot: 40,
        contents: ItemStack::new(ItemKind::Shield, 1),
    });
    s.tick();
    s.with_component(|inv: &Inventory| {
        assert_eq!(
            inv.inventory_menu.slot(45),
            Some(&ItemStack::new(ItemKind::Shield, 1)),
        );
    });

    // Open a Generic9x3 container; subsequent storage / hotbar writes
    // must mirror into its player_slots_range.
    s.receive_packet(ClientboundOpenScreen {
        container_id: 1,
        menu_type: MenuKind::Generic9x3,
        title: FormattedText::default(),
    });
    s.tick();
    s.receive_packet(ClientboundSetPlayerInventory {
        slot: 10,
        contents: ItemStack::new(ItemKind::Cobblestone, 64),
    });
    s.tick();
    s.with_component(|inv: &Inventory| {
        assert_eq!(
            inv.inventory_menu.slot(10),
            Some(&ItemStack::new(ItemKind::Cobblestone, 64)),
        );
        let container = inv
            .container_menu
            .as_ref()
            .expect("container should be open");
        let player_start = *container.player_slots_range().start();
        // vanilla storage slot 10 is the second storage slot; in the
        // container's player_slots ([storage(27) ++ hotbar(9)]) that's
        // index 1.
        assert_eq!(
            container.slot(player_start + 1),
            Some(&ItemStack::new(ItemKind::Cobblestone, 64)),
        );
    });

    // Out-of-range slot is ignored (no panic, nothing changes).
    s.receive_packet(ClientboundSetPlayerInventory {
        slot: 99,
        contents: ItemStack::new(ItemKind::Stone, 1),
    });
    s.tick();
}

/// Regression for the close-container path: when a container is open,
/// `SetPlayerInventory` writes for hotbar / storage must mirror into the
/// container menu so closing the container (which copies its
/// `player_slots_range` back into `inventory_menu`) doesn't clobber the
/// update with a stale value.
#[test]
fn test_set_player_inventory_survives_container_close() {
    let _lock = init();

    let mut s = Simulation::new(ConnectionProtocol::Game);
    s.receive_packet(default_login_packet());
    s.tick();

    s.receive_packet(ClientboundOpenScreen {
        container_id: 1,
        menu_type: MenuKind::Generic9x3,
        title: FormattedText::default(),
    });
    s.tick();

    let stone = ItemStack::new(ItemKind::Stone, 32);
    let iron = ItemStack::new(ItemKind::IronIngot, 4);
    // storage slot 9 (vanilla) -> menu protocol index 9
    s.receive_packet(ClientboundSetPlayerInventory {
        slot: 9,
        contents: stone.clone(),
    });
    // hotbar slot 0 (vanilla) -> menu protocol index 36
    s.receive_packet(ClientboundSetPlayerInventory {
        slot: 0,
        contents: iron.clone(),
    });
    s.tick();

    // close the container; vanilla copies container.player_slots_range
    // back into inventory_menu, so without the mirror the writes above
    // would be lost.
    s.receive_packet(ClientboundContainerClose { container_id: 1 });
    s.tick();

    s.with_component(|inv: &Inventory| {
        assert!(inv.container_menu.is_none());
        assert_eq!(inv.id, 0);
        assert_eq!(inv.inventory_menu.slot(9), Some(&stone));
        assert_eq!(inv.inventory_menu.slot(36), Some(&iron));
        assert_eq!(inv.held_item(), &iron);
    });
}
