use std::{cmp, collections::HashSet};

use azalea_chat::FormattedText;
use azalea_inventory::{
    ItemStack, ItemStackData, Menu,
    components::EquipmentSlot,
    item::MaxStackSizeExt,
    operations::{
        ClickOperation, CloneClick, PickupAllClick, PickupClick, QuickCraftKind, QuickCraftStatus,
        QuickCraftStatusKind, QuickMoveClick, ThrowClick,
    },
};

use crate::PlayerAbilities;

/// A local player's inventory and related data, including the container that
/// they may have opened.
#[cfg_attr(feature = "bevy_ecs", derive(bevy_ecs::component::Component))]
#[derive(Clone, Debug)]
pub struct Inventory {
    /// The player's inventory menu. This is guaranteed to be a `Menu::Player`.
    ///
    /// We keep it as a [`Menu`] since `Menu` has some useful functions that
    /// bare [`azalea_inventory::Player`] doesn't have.
    pub inventory_menu: azalea_inventory::Menu,

    /// The ID of the container that's currently open.
    ///
    /// Its value is not guaranteed to be anything specific, and it may change
    /// every time you open a container (unless it's 0, in which case it
    /// means that no container is open).
    pub id: i32,
    /// The current container menu that the player has open, or `None` if no
    /// container is open.
    pub container_menu: Option<azalea_inventory::Menu>,
    /// The custom name of the menu that's currently open.
    ///
    /// This can only be `Some` when `container_menu` is `Some`.
    pub container_menu_title: Option<FormattedText>,
    /// The item that is currently held by the cursor, or `Slot::Empty` if
    /// nothing is currently being held.
    ///
    /// This is different from [`Self::selected_hotbar_slot`], which is the
    /// item that's selected in the hotbar.
    pub carried: ItemStack,
    /// An identifier used by the server to track client inventory desyncs.
    ///
    /// This is sent on every container click, and it's only ever updated when
    /// the server sends a new container update.
    pub state_id: u32,

    pub quick_craft_status: QuickCraftStatusKind,
    pub quick_craft_kind: QuickCraftKind,
    /// A set of the indexes of the slots that have been right clicked in
    /// this "quick craft".
    pub quick_craft_slots: HashSet<u16>,

    /// The index of the item in the hotbar that's currently being held by the
    /// player. This must be in the range 0..=8.
    ///
    /// In a vanilla client this is changed by pressing the number keys or using
    /// the scroll wheel.
    pub selected_hotbar_slot: u8,
}

impl Inventory {
    /// Returns a reference to the currently active menu.
    ///
    /// If a container is open then it'll return [`Self::container_menu`],
    /// otherwise [`Self::inventory_menu`].
    ///
    /// Use [`Self::menu_mut`] if you need a mutable reference.
    pub fn menu(&self) -> &azalea_inventory::Menu {
        match &self.container_menu {
            Some(menu) => menu,
            _ => &self.inventory_menu,
        }
    }

    /// Returns a mutable reference to the currently active menu.
    ///
    /// If a container is open then it'll return [`Self::container_menu`],
    /// otherwise [`Self::inventory_menu`].
    ///
    /// Use [`Self::menu`] if you don't need a mutable reference.
    pub fn menu_mut(&mut self) -> &mut azalea_inventory::Menu {
        match &mut self.container_menu {
            Some(menu) => menu,
            _ => &mut self.inventory_menu,
        }
    }

    /// Modify the inventory as if the given operation was performed on it.
    pub fn simulate_click(
        &mut self,
        operation: &ClickOperation,
        player_abilities: &PlayerAbilities,
    ) {
        if let ClickOperation::QuickCraft(quick_craft) = operation {
            let last_quick_craft_status_tmp = self.quick_craft_status.clone();
            self.quick_craft_status = last_quick_craft_status_tmp.clone();
            let last_quick_craft_status = last_quick_craft_status_tmp;

            // no carried item, reset
            if self.carried.is_empty() {
                return self.reset_quick_craft();
            }
            // if we were starting or ending, or now we aren't ending and the status
            // changed, reset
            if (last_quick_craft_status == QuickCraftStatusKind::Start
                || last_quick_craft_status == QuickCraftStatusKind::End
                || self.quick_craft_status != QuickCraftStatusKind::End)
                && (self.quick_craft_status != last_quick_craft_status)
            {
                return self.reset_quick_craft();
            }
            if self.quick_craft_status == QuickCraftStatusKind::Start {
                self.quick_craft_kind = quick_craft.kind.clone();
                if self.quick_craft_kind == QuickCraftKind::Middle && player_abilities.instant_break
                {
                    self.quick_craft_status = QuickCraftStatusKind::Add;
                    self.quick_craft_slots.clear();
                } else {
                    self.reset_quick_craft();
                }
                return;
            }
            if let QuickCraftStatus::Add { slot } = quick_craft.status {
                let slot_item = self.menu().slot(slot as usize);
                if let Some(slot_item) = slot_item
                    && let ItemStack::Present(carried) = &self.carried
                {
                    // minecraft also checks slot.may_place(carried) and
                    // menu.can_drag_to(slot)
                    // but they always return true so they're not relevant for us
                    if can_item_quick_replace(slot_item, &self.carried, true)
                        && (self.quick_craft_kind == QuickCraftKind::Right
                            || carried.count as usize > self.quick_craft_slots.len())
                    {
                        self.quick_craft_slots.insert(slot);
                    }
                }
                return;
            }
            if self.quick_craft_status == QuickCraftStatusKind::End {
                if !self.quick_craft_slots.is_empty() {
                    if self.quick_craft_slots.len() == 1 {
                        // if we only clicked one slot, then turn this
                        // QuickCraftClick into a PickupClick
                        let slot = *self.quick_craft_slots.iter().next().unwrap();
                        self.reset_quick_craft();
                        self.simulate_click(
                            &match self.quick_craft_kind {
                                QuickCraftKind::Left => {
                                    PickupClick::Left { slot: Some(slot) }.into()
                                }
                                QuickCraftKind::Right => {
                                    PickupClick::Left { slot: Some(slot) }.into()
                                }
                                QuickCraftKind::Middle => {
                                    // idk just do nothing i guess
                                    return;
                                }
                            },
                            player_abilities,
                        );
                        return;
                    }

                    let ItemStack::Present(mut carried) = self.carried.clone() else {
                        // this should never happen
                        return self.reset_quick_craft();
                    };

                    let mut carried_count = carried.count;
                    let mut quick_craft_slots_iter = self.quick_craft_slots.iter();

                    loop {
                        let mut slot: &ItemStack;
                        let mut slot_index: u16;
                        let mut item_stack: &ItemStack;

                        loop {
                            let Some(&next_slot) = quick_craft_slots_iter.next() else {
                                carried.count = carried_count;
                                self.carried = ItemStack::Present(carried);
                                return self.reset_quick_craft();
                            };

                            slot = self.menu().slot(next_slot as usize).unwrap();
                            slot_index = next_slot;
                            item_stack = &self.carried;

                            if slot.is_present()
                                    && can_item_quick_replace(slot, item_stack, true)
                                    // this always returns true in most cases
                                    // && slot.may_place(item_stack)
                                    && (
                                        self.quick_craft_kind == QuickCraftKind::Middle
                                        || item_stack.count()  >= self.quick_craft_slots.len() as i32
                                    )
                            {
                                break;
                            }
                        }

                        // get the ItemStackData for the slot
                        let ItemStack::Present(slot) = slot else {
                            unreachable!("the loop above requires the slot to be present to break")
                        };

                        // if self.can_drag_to(slot) {
                        let mut new_carried = carried.clone();
                        let slot_item_count = slot.count;
                        get_quick_craft_slot_count(
                            &self.quick_craft_slots,
                            &self.quick_craft_kind,
                            &mut new_carried,
                            slot_item_count,
                        );
                        let max_stack_size = i32::min(
                            new_carried.kind.max_stack_size(),
                            i32::min(
                                new_carried.kind.max_stack_size(),
                                slot.kind.max_stack_size(),
                            ),
                        );
                        if new_carried.count > max_stack_size {
                            new_carried.count = max_stack_size;
                        }

                        carried_count -= new_carried.count - slot_item_count;
                        // we have to inline self.menu_mut() here to avoid the borrow checker
                        // complaining
                        let menu = match &mut self.container_menu {
                            Some(menu) => menu,
                            _ => &mut self.inventory_menu,
                        };
                        *menu.slot_mut(slot_index as usize).unwrap() =
                            ItemStack::Present(new_carried);
                    }
                }
            } else {
                return self.reset_quick_craft();
            }
        }
        // the quick craft status should always be in start if we're not in quick craft
        // mode
        if self.quick_craft_status != QuickCraftStatusKind::Start {
            return self.reset_quick_craft();
        }

        match operation {
            // left clicking outside inventory
            ClickOperation::Pickup(PickupClick::Left { slot: None })
                if self.carried.is_present() =>
            {
                // vanilla has `player.drop`s but they're only used
                // server-side
                // they're included as comments here in case you want to adapt this for a server
                // implementation

                // player.drop(self.carried, true);
                self.carried = ItemStack::Empty;
            }
            ClickOperation::Pickup(PickupClick::Right { slot: None })
                if self.carried.is_present() =>
            {
                let _item = self.carried.split(1);
                // player.drop(item, true);
            }
            &ClickOperation::Pickup(
                // lol
                ref pickup @ (PickupClick::Left { slot: Some(slot) }
                | PickupClick::Right { slot: Some(slot) }),
            ) => {
                let slot = slot as usize;
                let Some(slot_item) = self.menu().slot(slot) else {
                    return;
                };

                if self.try_item_click_behavior_override(operation, slot) {
                    return;
                }

                let is_left_click = matches!(pickup, PickupClick::Left { .. });

                match slot_item {
                    ItemStack::Empty => {
                        if self.carried.is_present() {
                            let place_count = if is_left_click {
                                self.carried.count()
                            } else {
                                1
                            };
                            self.carried =
                                self.safe_insert(slot, self.carried.clone(), place_count);
                        }
                    }
                    ItemStack::Present(_) => {
                        if !self.menu().may_pickup(slot) {
                            return;
                        }
                        if let ItemStack::Present(carried) = self.carried.clone() {
                            let slot_is_same_item_as_carried = slot_item
                                .as_present()
                                .is_some_and(|s| carried.is_same_item_and_components(s));

                            if self.menu().may_place(slot, &carried) {
                                if slot_is_same_item_as_carried {
                                    let place_count = if is_left_click { carried.count } else { 1 };
                                    self.carried =
                                        self.safe_insert(slot, self.carried.clone(), place_count);
                                } else if carried.count
                                    <= self
                                        .menu()
                                        .max_stack_size(slot)
                                        .min(carried.kind.max_stack_size())
                                {
                                    // swap slot_item and carried
                                    self.carried = slot_item.clone();
                                    let slot_item = self.menu_mut().slot_mut(slot).unwrap();
                                    *slot_item = carried.into();
                                }
                            } else if slot_is_same_item_as_carried
                                && let Some(removed) = self.try_remove(
                                    slot,
                                    slot_item.count(),
                                    carried.kind.max_stack_size() - carried.count,
                                )
                            {
                                self.carried.as_present_mut().unwrap().count += removed.count();
                                // slot.onTake(player, removed);
                            }
                        } else {
                            let pickup_count = if is_left_click {
                                slot_item.count()
                            } else {
                                (slot_item.count() + 1) / 2
                            };
                            if let Some(new_slot_item) =
                                self.try_remove(slot, pickup_count, i32::MAX)
                            {
                                self.carried = new_slot_item;
                                // slot.onTake(player, newSlot);
                            }
                        }
                    }
                }
            }
            &ClickOperation::QuickMove(
                QuickMoveClick::Left { slot } | QuickMoveClick::Right { slot },
            ) => {
                // in vanilla it also tests if QuickMove has a slot index of -999
                // but i don't think that's ever possible so it's not covered here
                let slot = slot as usize;
                loop {
                    let new_slot_item = self.menu_mut().quick_move_stack(slot);
                    let slot_item = self.menu().slot(slot).unwrap();
                    if new_slot_item.is_empty() || slot_item.kind() != new_slot_item.kind() {
                        break;
                    }
                }
            }
            ClickOperation::Swap(s) => {
                let source_slot_index = s.source_slot as usize;
                // `s.target_slot` is the *wire* button: 0..=8 = hotbar, 40 =
                // offhand. It is **not** a menu protocol index. Translate it
                // into the currently-active menu's protocol index. The
                // exception is `40` (offhand) when a non-player container is
                // open: vanilla still applies the swap via the raw player
                // inventory, but the active menu has no slot for offhand —
                // handle that directly against `inventory_menu` and bail out
                // before the active-menu fallthrough.
                if s.target_slot == 40 && !matches!(self.menu(), Menu::Player(_)) {
                    self.simulate_swap_with_inventory_offhand(source_slot_index);
                    return;
                }
                let Some(target_slot_index) =
                    self.swap_button_to_menu_protocol_index(s.target_slot)
                else {
                    return;
                };

                let Some(source_slot) = self.menu().slot(source_slot_index) else {
                    return;
                };
                let Some(target_slot) = self.menu().slot(target_slot_index) else {
                    return;
                };
                if source_slot.is_empty() && target_slot.is_empty() {
                    return;
                }

                if target_slot.is_empty() {
                    if self.menu().may_pickup(source_slot_index) {
                        let source_slot = source_slot.clone();
                        let target_slot = self.menu_mut().slot_mut(target_slot_index).unwrap();
                        *target_slot = source_slot;
                    }
                } else if source_slot.is_empty() {
                    let target_item = target_slot
                        .as_present()
                        .expect("target slot was already checked to not be empty");
                    if self.menu().may_place(source_slot_index, target_item) {
                        // get the target_item but mutable
                        let source_max_stack_size = self.menu().max_stack_size(source_slot_index);

                        let target_slot = self.menu_mut().slot_mut(target_slot_index).unwrap();
                        let new_source_slot =
                            target_slot.split(source_max_stack_size.try_into().unwrap());
                        *self.menu_mut().slot_mut(source_slot_index).unwrap() = new_source_slot;
                    }
                } else if self.menu().may_pickup(source_slot_index) {
                    let ItemStack::Present(target_item) = target_slot else {
                        unreachable!("target slot is not empty but is not present");
                    };
                    if self.menu().may_place(source_slot_index, target_item) {
                        let source_max_stack = self.menu().max_stack_size(source_slot_index);
                        if target_slot.count() > source_max_stack {
                            // if there's more than the max stack size in the target slot

                            let target_slot = self.menu_mut().slot_mut(target_slot_index).unwrap();
                            let new_source_slot =
                                target_slot.split(source_max_stack.try_into().unwrap());
                            *self.menu_mut().slot_mut(source_slot_index).unwrap() = new_source_slot;
                            // if !self.inventory_menu.add(new_source_slot) {
                            //     player.drop(new_source_slot, true);
                            // }
                        } else {
                            // normal swap
                            let new_target_slot = source_slot.clone();
                            let new_source_slot = target_slot.clone();

                            let target_slot = self.menu_mut().slot_mut(target_slot_index).unwrap();
                            *target_slot = new_target_slot;

                            let source_slot = self.menu_mut().slot_mut(source_slot_index).unwrap();
                            *source_slot = new_source_slot;
                        }
                    }
                }
            }
            ClickOperation::Clone(CloneClick { slot }) => {
                if !player_abilities.instant_break || self.carried.is_present() {
                    return;
                }
                let Some(source_slot) = self.menu().slot(*slot as usize) else {
                    return;
                };
                let ItemStack::Present(source_item) = source_slot else {
                    return;
                };
                let mut new_carried = source_item.clone();
                new_carried.count = new_carried.kind.max_stack_size();
                self.carried = ItemStack::Present(new_carried);
            }
            ClickOperation::Throw(c) => {
                if self.carried.is_present() {
                    return;
                }

                let (ThrowClick::Single { slot: slot_index }
                | ThrowClick::All { slot: slot_index }) = c;
                let slot_index = *slot_index as usize;

                let Some(slot) = self.menu_mut().slot_mut(slot_index) else {
                    return;
                };
                let ItemStack::Present(slot_item) = slot else {
                    return;
                };

                let dropping_count = match c {
                    ThrowClick::Single { .. } => 1,
                    ThrowClick::All { .. } => slot_item.count,
                };

                let _dropping = slot_item.split(dropping_count as u32);
                // player.drop(dropping, true);
            }
            ClickOperation::PickupAll(PickupAllClick {
                slot: source_slot_index,
                reversed,
            }) => {
                let source_slot_index = *source_slot_index as usize;

                let source_slot = self.menu().slot(source_slot_index).unwrap();
                let target_slot = self.carried.clone();

                if target_slot.is_empty()
                    || (source_slot.is_present() && self.menu().may_pickup(source_slot_index))
                {
                    return;
                }

                let ItemStack::Present(target_slot_item) = &target_slot else {
                    unreachable!("target slot is not empty but is not present");
                };

                for round in 0..2 {
                    let iterator: Box<dyn Iterator<Item = usize>> = if *reversed {
                        Box::new((0..self.menu().len()).rev())
                    } else {
                        Box::new(0..self.menu().len())
                    };

                    for i in iterator {
                        if target_slot_item.count < target_slot_item.kind.max_stack_size() {
                            let checking_slot = self.menu().slot(i).unwrap();
                            if let ItemStack::Present(checking_item) = checking_slot
                                && can_item_quick_replace(checking_slot, &target_slot, true)
                                && self.menu().may_pickup(i)
                                && (round != 0
                                    || checking_item.count != checking_item.kind.max_stack_size())
                            {
                                // get the checking_slot and checking_item again but mutable
                                let checking_slot = self.menu_mut().slot_mut(i).unwrap();

                                let taken_item = checking_slot.split(checking_slot.count() as u32);

                                // now extend the carried item
                                let target_slot = &mut self.carried;
                                let ItemStack::Present(target_slot_item) = target_slot else {
                                    unreachable!("target slot is not empty but is not present");
                                };
                                target_slot_item.count += taken_item.count();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn reset_quick_craft(&mut self) {
        self.quick_craft_status = QuickCraftStatusKind::Start;
        self.quick_craft_slots.clear();
    }

    /// Get the item in the player's hotbar that is currently being held in
    /// their main hand.
    pub fn held_item(&self) -> &ItemStack {
        self.get_equipment(EquipmentSlot::Mainhand)
            .expect("The main hand item should always be present")
    }

    /// Translate a vanilla player-inventory slot index (as used by
    /// `ClientboundSetPlayerInventory` and vanilla
    /// `net.minecraft.world.entity.player.Inventory#setItem`) to the
    /// `Menu::Player` protocol index.
    ///
    /// Vanilla layout (see `Inventory#setItem` in MC 1.21.5+):
    /// * `0..=8` — hotbar (left to right)
    /// * `9..=35` — main inventory storage (top-left to bottom-right)
    /// * `36..=39` — armor: 36 feet, 37 legs, 38 chest, 39 head
    /// * `40` — offhand
    ///
    /// `Menu::Player` layout: `0` craft result, `1..=4` craft grid,
    /// `5..=8` armor (head, chest, legs, feet), `9..=44` inventory
    /// (`9..=35` storage, `36..=44` hotbar), `45` offhand.
    ///
    /// Returns `None` if the vanilla slot index is out of range.
    pub fn player_inventory_slot_to_menu_protocol_index(slot: u32) -> Option<usize> {
        match slot {
            0..=8 => Some(*azalea_inventory::Player::HOTBAR_SLOTS.start() + slot as usize),
            9..=35 => Some(slot as usize),
            // 36 (feet) -> menu 8, 37 (legs) -> 7, 38 (chest) -> 6, 39 (head) -> 5
            36..=39 => Some(*azalea_inventory::Player::ARMOR_SLOTS.end() - (slot as usize - 36)),
            40 => Some(azalea_inventory::Player::OFFHAND_SLOT),
            _ => None,
        }
    }

    /// Handle `SwapClick { target_slot: 40 }` (offhand) while a non-player
    /// container is open. The active menu has no offhand slot — vanilla
    /// applies the swap via the raw `Inventory#setItem(40, ...)` API, so we
    /// mirror that by reading/writing `inventory_menu`'s offhand directly
    /// while still updating the active container menu's source slot.
    fn simulate_swap_with_inventory_offhand(&mut self, source_slot_index: usize) {
        if self.menu().slot(source_slot_index).is_none() {
            return;
        }
        let offhand_idx = azalea_inventory::Player::OFFHAND_SLOT;
        let Some(offhand_item) = self.inventory_menu.slot(offhand_idx) else {
            return;
        };
        let source_item = self.menu().slot(source_slot_index).unwrap().clone();
        let offhand_item = offhand_item.clone();
        if source_item.is_empty() && offhand_item.is_empty() {
            return;
        }
        // simple swap — `may_pickup` / `may_place` / `max_stack_size` are
        // permissive in this codebase (mirror of the existing swap branch's
        // pre-existing behavior).
        *self.menu_mut().slot_mut(source_slot_index).unwrap() = offhand_item;
        *self.inventory_menu.slot_mut(offhand_idx).unwrap() = source_item;
    }

    /// Translate a vanilla swap-click button (0..=8 hotbar, 40 offhand) to
    /// the currently-active menu's protocol index.
    ///
    /// Vanilla wire format for [`crate::ClickOperation::Swap`] reuses the
    /// `button` field for the destination: number keys 1-9 → 0-8 (hotbar),
    /// `F` → 40 (offhand). When the player has a container open the swap
    /// target stays in player inventory, but the protocol index of those
    /// slots depends on the active menu (e.g. `Generic9x3`'s hotbar starts at
    /// `player_slots_range().start() + 27`).
    ///
    /// Returns `None` if `button` is not a valid swap target for the active
    /// menu (e.g. `40`/offhand while a non-player container is open — vanilla
    /// containers don't expose offhand, the server still applies it via raw
    /// inventory write but the active menu has no slot to mirror).
    fn swap_button_to_menu_protocol_index(&self, button: u8) -> Option<usize> {
        match button {
            0..=8 => {
                // hotbar is the last 9 slots of the active menu's
                // player_slots_range (vanilla `Inventory` lays out player_slots
                // as `[storage(27) ++ hotbar(9)]`).
                let hotbar = self.menu().hotbar_slots_range();
                Some(*hotbar.start() + button as usize)
            }
            40 => {
                // offhand only exists in `Menu::Player`
                if matches!(self.menu(), Menu::Player(_)) {
                    Some(azalea_inventory::Player::OFFHAND_SLOT)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Apply a `ClientboundSetPlayerInventory`-style update: write the given
    /// item into the player inventory at the given vanilla slot index. Updates
    /// `inventory_menu`, and (for hotbar / storage slots) also mirrors into
    /// the open `container_menu`'s player slots — vanilla aliases those slots
    /// to the same `ItemStack` instances as `Inventory`.
    ///
    /// Returns `true` if the slot index was valid and the write happened.
    pub fn set_player_inventory_slot(&mut self, slot: u32, item: ItemStack) -> bool {
        let Some(menu_idx) = Self::player_inventory_slot_to_menu_protocol_index(slot) else {
            return false;
        };
        let Some(target) = self.inventory_menu.slot_mut(menu_idx) else {
            return false;
        };
        *target = item.clone();

        // Mirror storage+hotbar (vanilla slot 0..=35) into the open container
        // menu's player_slots_range. Armor (36..=39) and offhand (40) aren't
        // visible in container UIs and aren't mirrored.
        if slot <= 35
            && let Some(container) = self.container_menu.as_mut()
        {
            let player_slots = container.player_slots_range();
            // Vanilla `Inventory` lays out player_slots as
            // `[storage(27) ++ hotbar(9)]` regardless of container kind, so
            // the mapping from vanilla slot is direct.
            let offset = if slot <= 8 {
                // hotbar -> last 9 of player_slots
                27 + slot as usize
            } else {
                // storage 9..=35 -> first 27 of player_slots
                slot as usize - 9
            };
            if let Some(target) = container.slot_mut(*player_slots.start() + offset) {
                *target = item;
            }
        }
        true
    }

    /// TODO: implement bundles
    fn try_item_click_behavior_override(
        &self,
        _operation: &ClickOperation,
        _slot_item_index: usize,
    ) -> bool {
        false
    }

    fn safe_insert(&mut self, slot: usize, src_item: ItemStack, take_count: i32) -> ItemStack {
        let Some(slot_item) = self.menu_mut().slot_mut(slot) else {
            return src_item;
        };
        let ItemStack::Present(mut src_item) = src_item else {
            return src_item;
        };

        let take_count = cmp::min(
            cmp::min(take_count, src_item.count),
            src_item.kind.max_stack_size() - slot_item.count(),
        );
        if take_count <= 0 {
            return src_item.into();
        }
        let take_count = take_count as u32;

        if slot_item.is_empty() {
            *slot_item = src_item.split(take_count).into();
        } else if let ItemStack::Present(slot_item) = slot_item
            && slot_item.is_same_item_and_components(&src_item)
        {
            src_item.count -= take_count as i32;
            slot_item.count += take_count as i32;
        }

        src_item.into()
    }

    fn try_remove(&mut self, slot: usize, count: i32, limit: i32) -> Option<ItemStack> {
        if !self.menu().may_pickup(slot) {
            return None;
        }
        let mut slot_item = self.menu().slot(slot)?.clone();
        if !self.menu().allow_modification(slot) && limit < slot_item.count() {
            return None;
        }

        let count = count.min(limit);
        if count <= 0 {
            return None;
        }
        // vanilla calls .remove here but i think it has the same behavior as split?
        let removed = slot_item.split(count as u32);

        if removed.is_present() && slot_item.is_empty() {
            *self.menu_mut().slot_mut(slot).unwrap() = ItemStack::Empty;
        }

        Some(removed)
    }

    /// Get the item at the given equipment slot, or `None` if the inventory
    /// can't contain that slot.
    pub fn get_equipment(&self, equipment_slot: EquipmentSlot) -> Option<&ItemStack> {
        let player = self.inventory_menu.as_player();
        let item = match equipment_slot {
            EquipmentSlot::Mainhand => {
                let menu = self.menu();
                let main_hand_slot_idx =
                    *menu.hotbar_slots_range().start() + self.selected_hotbar_slot as usize;
                menu.slot(main_hand_slot_idx)?
            }
            EquipmentSlot::Offhand => &player.offhand,
            EquipmentSlot::Feet => &player.armor[3],
            EquipmentSlot::Legs => &player.armor[2],
            EquipmentSlot::Chest => &player.armor[1],
            EquipmentSlot::Head => &player.armor[0],
            EquipmentSlot::Body => {
                // TODO: when riding entities is implemented, mount/horse inventories should be
                // implemented too. note that horse inventories aren't a normal menu (they're
                // not in MenuKind), maybe they should be a separate field in `Inventory`?
                return None;
            }
            EquipmentSlot::Saddle => {
                // TODO: implement riding entities, see above
                return None;
            }
        };
        Some(item)
    }
}

fn can_item_quick_replace(
    target_slot: &ItemStack,
    item: &ItemStack,
    ignore_item_count: bool,
) -> bool {
    let ItemStack::Present(target_slot) = target_slot else {
        return false;
    };
    let ItemStack::Present(item) = item else {
        // i *think* this is what vanilla does
        // not 100% sure lol probably doesn't matter though
        return false;
    };

    if !item.is_same_item_and_components(target_slot) {
        return false;
    }
    let count = target_slot.count as u16
        + if ignore_item_count {
            0
        } else {
            item.count as u16
        };
    count <= item.kind.max_stack_size() as u16
}

fn get_quick_craft_slot_count(
    quick_craft_slots: &HashSet<u16>,
    quick_craft_kind: &QuickCraftKind,
    item: &mut ItemStackData,
    slot_item_count: i32,
) {
    item.count = match quick_craft_kind {
        QuickCraftKind::Left => item.count / quick_craft_slots.len() as i32,
        QuickCraftKind::Right => 1,
        QuickCraftKind::Middle => item.kind.max_stack_size(),
    };
    item.count += slot_item_count;
}

impl Default for Inventory {
    fn default() -> Self {
        Inventory {
            inventory_menu: Menu::Player(azalea_inventory::Player::default()),
            id: 0,
            container_menu: None,
            container_menu_title: None,
            carried: ItemStack::Empty,
            state_id: 0,
            quick_craft_status: QuickCraftStatusKind::Start,
            quick_craft_kind: QuickCraftKind::Middle,
            quick_craft_slots: HashSet::new(),
            selected_hotbar_slot: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use azalea_inventory::SlotList;
    use azalea_registry::builtin::ItemKind;

    use super::*;

    #[test]
    fn test_player_inventory_slot_to_menu_protocol_index() {
        // hotbar
        for s in 0..=8 {
            assert_eq!(
                Inventory::player_inventory_slot_to_menu_protocol_index(s),
                Some(36 + s as usize),
                "hotbar slot {s}",
            );
        }
        // storage
        for s in 9..=35 {
            assert_eq!(
                Inventory::player_inventory_slot_to_menu_protocol_index(s),
                Some(s as usize),
                "storage slot {s}",
            );
        }
        // armor: 36 feet -> 8, 37 legs -> 7, 38 chest -> 6, 39 head -> 5
        assert_eq!(
            Inventory::player_inventory_slot_to_menu_protocol_index(36),
            Some(8),
        );
        assert_eq!(
            Inventory::player_inventory_slot_to_menu_protocol_index(37),
            Some(7),
        );
        assert_eq!(
            Inventory::player_inventory_slot_to_menu_protocol_index(38),
            Some(6),
        );
        assert_eq!(
            Inventory::player_inventory_slot_to_menu_protocol_index(39),
            Some(5),
        );
        // offhand
        assert_eq!(
            Inventory::player_inventory_slot_to_menu_protocol_index(40),
            Some(45),
        );
        // out of range
        assert_eq!(
            Inventory::player_inventory_slot_to_menu_protocol_index(41),
            None,
        );
    }

    #[test]
    fn test_set_player_inventory_slot_writes_inventory_menu() {
        let mut inventory = Inventory::default();
        let stone = ItemStack::new(ItemKind::Stone, 1);

        // hotbar slot 0 -> menu index 36
        assert!(inventory.set_player_inventory_slot(0, stone.clone()));
        assert_eq!(inventory.inventory_menu.slot(36), Some(&stone));

        // storage slot 9 -> menu index 9
        assert!(inventory.set_player_inventory_slot(9, stone.clone()));
        assert_eq!(inventory.inventory_menu.slot(9), Some(&stone));

        // armor slot 39 (head) -> menu index 5
        assert!(inventory.set_player_inventory_slot(39, stone.clone()));
        assert_eq!(inventory.inventory_menu.slot(5), Some(&stone));

        // offhand slot 40 -> menu index 45
        assert!(inventory.set_player_inventory_slot(40, stone.clone()));
        assert_eq!(inventory.inventory_menu.slot(45), Some(&stone));

        // out of range
        assert!(!inventory.set_player_inventory_slot(41, stone));
    }

    #[test]
    fn test_set_player_inventory_slot_mirrors_open_container() {
        // simulate having a Generic9x3 container open on top of the player
        // inventory; storage / hotbar writes should mirror into the container
        // menu's player_slots_range, and armor / offhand writes shouldn't.
        let mut inventory = Inventory {
            inventory_menu: Menu::Player(azalea_inventory::Player::default()),
            id: 1,
            container_menu: Some(Menu::Generic9x3 {
                contents: SlotList::default(),
                player: SlotList::default(),
            }),
            container_menu_title: None,
            carried: ItemStack::Empty,
            state_id: 0,
            quick_craft_status: QuickCraftStatusKind::Start,
            quick_craft_kind: QuickCraftKind::Middle,
            quick_craft_slots: HashSet::new(),
            selected_hotbar_slot: 0,
        };
        let stone = ItemStack::new(ItemKind::Stone, 1);

        // storage slot 9 (vanilla) -> player.inventory[0] (menu 9 in player
        // menu); in Generic9x3 the player_slots_range starts after the 27
        // chest contents, and the first 27 are storage.
        assert!(inventory.set_player_inventory_slot(9, stone.clone()));
        let container_player_start = *inventory
            .container_menu
            .as_ref()
            .unwrap()
            .player_slots_range()
            .start();
        assert_eq!(
            inventory
                .container_menu
                .as_ref()
                .unwrap()
                .slot(container_player_start),
            Some(&stone),
        );
        // and inventory_menu still got it
        assert_eq!(inventory.inventory_menu.slot(9), Some(&stone));

        // hotbar slot 0 (vanilla) -> last 9 of player_slots_range
        let hotbar_iron = ItemStack::new(ItemKind::IronIngot, 5);
        assert!(inventory.set_player_inventory_slot(0, hotbar_iron.clone()));
        assert_eq!(
            inventory
                .container_menu
                .as_ref()
                .unwrap()
                .slot(container_player_start + 27),
            Some(&hotbar_iron),
        );

        // armor write doesn't touch the container menu
        let helm = ItemStack::new(ItemKind::IronHelmet, 1);
        assert!(inventory.set_player_inventory_slot(39, helm.clone()));
        assert_eq!(inventory.inventory_menu.slot(5), Some(&helm));
        // container menu has no armor mirror — its slot 5 is still chest
        // contents (Empty since we set it that way).
        assert_eq!(
            inventory.container_menu.as_ref().unwrap().slot(5),
            Some(&ItemStack::Empty),
        );
    }

    #[test]
    fn test_simulate_shift_click_in_crafting_table() {
        let spruce_planks = ItemStack::new(ItemKind::SprucePlanks, 4);

        let mut inventory = Inventory {
            inventory_menu: Menu::Player(azalea_inventory::Player::default()),
            id: 1,
            container_menu: Some(Menu::Crafting {
                result: spruce_planks.clone(),
                // simulate_click won't delete the items from here
                grid: SlotList::default(),
                player: SlotList::default(),
            }),
            container_menu_title: None,
            carried: ItemStack::Empty,
            state_id: 0,
            quick_craft_status: QuickCraftStatusKind::Start,
            quick_craft_kind: QuickCraftKind::Middle,
            quick_craft_slots: HashSet::new(),
            selected_hotbar_slot: 0,
        };

        inventory.simulate_click(
            &ClickOperation::QuickMove(QuickMoveClick::Left { slot: 0 }),
            &PlayerAbilities::default(),
        );

        let new_slots = inventory.menu().slots();
        assert_eq!(&new_slots[0], &ItemStack::Empty);
        assert_eq!(
            &new_slots[*Menu::CRAFTING_PLAYER_SLOTS.start()],
            &spruce_planks
        );
    }

    /// Pressing number key `n` (1-9) in the player inventory while hovering
    /// over a storage slot should swap the storage slot with hotbar slot
    /// `n - 1` (vanilla wire button = `n - 1` ∈ 0..=8). The previous prediction
    /// treated `button` as a menu protocol index and wrote the swap into the
    /// wrong slot.
    #[test]
    fn test_simulate_swap_hotbar_button_in_player_menu() {
        use azalea_inventory::operations::SwapClick;

        // start with a stone in storage slot 9 (top-left of inventory storage)
        // and an iron ingot in hotbar slot 0 (menu protocol index 36).
        let stone = ItemStack::new(ItemKind::Stone, 1);
        let iron = ItemStack::new(ItemKind::IronIngot, 5);

        let mut player = azalea_inventory::Player::default();
        player.inventory[0] = stone.clone();
        player.inventory[27] = iron.clone();

        let mut inventory = Inventory {
            inventory_menu: Menu::Player(player),
            ..Inventory::default()
        };

        // Press "1" while hovering storage slot 9 (menu index 9). Wire button
        // is 0 → hotbar slot 0 → menu index 36.
        inventory.simulate_click(
            &ClickOperation::Swap(SwapClick {
                source_slot: 9,
                target_slot: 0,
            }),
            &PlayerAbilities::default(),
        );

        assert_eq!(
            inventory.inventory_menu.slot(9),
            Some(&iron),
            "storage slot got the hotbar item",
        );
        assert_eq!(
            inventory.inventory_menu.slot(36),
            Some(&stone),
            "hotbar slot got the storage item",
        );
    }

    /// Press `F` while hovering a storage slot that holds the same item as
    /// offhand — wire button = 40 → offhand (menu protocol index 45 in
    /// `Menu::Player`). Uses the both-sides-present branch so we test the
    /// button→protocol translation independently of the (pre-existing)
    /// target-empty branch source-clear bug.
    #[test]
    fn test_simulate_swap_offhand_button_in_player_menu() {
        use azalea_inventory::operations::SwapClick;

        let stone = ItemStack::new(ItemKind::Stone, 1);
        let iron = ItemStack::new(ItemKind::IronIngot, 5);
        let mut player = azalea_inventory::Player::default();
        player.inventory[0] = stone.clone();
        player.offhand = iron.clone();

        let mut inventory = Inventory {
            inventory_menu: Menu::Player(player),
            ..Inventory::default()
        };

        inventory.simulate_click(
            &ClickOperation::Swap(SwapClick {
                source_slot: 9,
                target_slot: 40,
            }),
            &PlayerAbilities::default(),
        );

        // Both items swapped — proves button=40 mapped to menu index 45
        // (offhand) and not to nothing / wrong slot.
        assert_eq!(inventory.inventory_menu.slot(9), Some(&iron));
        assert_eq!(inventory.inventory_menu.slot(45), Some(&stone));
    }

    /// Number-key swap inside an open container (e.g. `Generic9x3`) should
    /// resolve to the container menu's own hotbar range, not raw protocol
    /// index `0..=8` which lives inside chest contents.
    #[test]
    fn test_simulate_swap_hotbar_button_in_container_menu() {
        use azalea_inventory::operations::SwapClick;

        let stone = ItemStack::new(ItemKind::Stone, 1);
        let iron = ItemStack::new(ItemKind::IronIngot, 5);

        // Build a Generic9x3 menu with stone in chest slot 0 and iron in
        // hotbar slot 0 (last 9 of player_slots: contents(27) + storage(27)
        // ... hotbar(9)).
        let mut chest = SlotList::default();
        chest[0] = stone.clone();
        let mut player_slots = SlotList::default();
        // first 27 = storage, last 9 = hotbar
        player_slots[27] = iron.clone();

        let mut inventory = Inventory {
            inventory_menu: Menu::Player(azalea_inventory::Player::default()),
            id: 1,
            container_menu: Some(Menu::Generic9x3 {
                contents: chest,
                player: player_slots,
            }),
            container_menu_title: None,
            carried: ItemStack::Empty,
            state_id: 0,
            quick_craft_status: QuickCraftStatusKind::Start,
            quick_craft_kind: QuickCraftKind::Middle,
            quick_craft_slots: HashSet::new(),
            selected_hotbar_slot: 0,
        };

        let menu_ref = inventory.container_menu.as_ref().unwrap();
        let hotbar_start = *menu_ref.hotbar_slots_range().start();
        // pre-condition sanity
        assert_eq!(menu_ref.slot(0), Some(&stone));
        assert_eq!(menu_ref.slot(hotbar_start), Some(&iron));

        // Press "1" hovering chest slot 0. Wire button 0 should map to the
        // container menu's hotbar slot 0 (= hotbar_start), NOT protocol
        // index 0 (which is chest slot 0 — the source itself).
        inventory.simulate_click(
            &ClickOperation::Swap(SwapClick {
                source_slot: 0,
                target_slot: 0,
            }),
            &PlayerAbilities::default(),
        );

        let menu_ref = inventory.container_menu.as_ref().unwrap();
        assert_eq!(menu_ref.slot(0), Some(&iron), "chest slot got hotbar item");
        assert_eq!(
            menu_ref.slot(hotbar_start),
            Some(&stone),
            "hotbar slot got chest item",
        );
    }

    /// `F` swap (button = 40) inside an open non-player container should still
    /// move items between the source slot and the player's offhand. Vanilla
    /// applies it via the raw `Inventory#setItem(40, ...)`; locally we mirror
    /// it through `inventory_menu`'s offhand because the container menu has
    /// no offhand slot.
    #[test]
    fn test_simulate_swap_offhand_button_in_container_menu() {
        use azalea_inventory::operations::SwapClick;

        let stone = ItemStack::new(ItemKind::Stone, 1);
        let iron = ItemStack::new(ItemKind::IronIngot, 5);

        let mut chest = SlotList::default();
        chest[0] = stone.clone();
        let mut player = azalea_inventory::Player::default();
        player.offhand = iron.clone();

        let mut inventory = Inventory {
            inventory_menu: Menu::Player(player),
            id: 1,
            container_menu: Some(Menu::Generic9x3 {
                contents: chest,
                player: SlotList::default(),
            }),
            container_menu_title: None,
            carried: ItemStack::Empty,
            state_id: 0,
            quick_craft_status: QuickCraftStatusKind::Start,
            quick_craft_kind: QuickCraftKind::Middle,
            quick_craft_slots: HashSet::new(),
            selected_hotbar_slot: 0,
        };

        inventory.simulate_click(
            &ClickOperation::Swap(SwapClick {
                source_slot: 0,
                target_slot: 40,
            }),
            &PlayerAbilities::default(),
        );

        // chest slot now has iron (from offhand); inventory_menu offhand has
        // stone (from chest).
        assert_eq!(
            inventory.container_menu.as_ref().unwrap().slot(0),
            Some(&iron),
        );
        assert_eq!(
            inventory.inventory_menu.slot(azalea_inventory::Player::OFFHAND_SLOT),
            Some(&stone),
        );
    }
}
