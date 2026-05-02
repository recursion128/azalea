//! Scoreboard 三个 packet（display slot / objective 注册 / 分数）→ ECS 接口。
//!
//! vanilla 维护一组**带名字的 objective**（`name → Objective { display_name,
//! criteria, number_format }`），加 19 个**显示槽位**（`list` / `sidebar` /
//! `below_name` + 16 个 team-color sidebar 变体），再 +
//! `(objective_name, holder) → score: i32` 二维表。本插件三组**per-player
//! Component** 按字段拆分，handler 各自独立 fold。
//!
//! ## 为什么是 Component 不是 Resource
//!
//! azalea swarm 一个 App 跑多 client，scoreboard 是**每连接私有**——A
//! client 看到的 sidebar 和 B client 不应串台。Component-on-LocalEntity
//! 与 [`Inventory`] 同语义。
//!
//! ## 协议层 packet 映射
//!
//! - `ClientboundSetDisplayObjective { slot: DisplaySlot, objective_name }`
//!   → 写 [`ScoreboardDisplay`] 对应槽位。**保留** vanilla 19 个 slot
//!   原值（`DisplaySlot` enum），不再 collapse 到 3——`Sidebar*` team-color
//!   变体在 vanilla 表示"只对该队成员显示"，UI 侧需要按 player team color
//!   选择实际显示的 sidebar 槽位。
//! - `ClientboundSetObjective { name, method }` → `Add` / `Change` 写
//!   [`ScoreboardObjectives`]，`Remove` 删；`Remove` 时同步清理
//!   [`ScoreboardScores`] 中所有该 objective 的 holder 记录。
//! - `ClientboundSetScore { owner, objective_name, score, .. }` →
//!   写 [`ScoreboardScores`] 的 `(objective, holder) → i32`。
//! - `ClientboundResetScore { owner, objective_name }` → 删一条；当
//!   `objective_name` 是 `None` 表示**删该 holder 在所有 objective 下**的分数
//!   （vanilla `clearPlayerScores` 语义）。

use std::collections::HashMap;

use azalea_chat::{FormattedText, numbers::NumberFormat};
use azalea_core::objectives::ObjectiveCriteria;
use azalea_protocol::packets::game::{
    c_reset_score::ClientboundResetScore,
    c_set_display_objective::{ClientboundSetDisplayObjective, DisplaySlot},
    c_set_objective::{ClientboundSetObjective, Method},
    c_set_score::ClientboundSetScore,
};
use bevy_app::{App, Plugin};
use bevy_ecs::{prelude::*, system::SystemState};

pub struct ScoreboardPlugin;

impl Plugin for ScoreboardPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ScoreboardChanged>();
    }
}

/// 19 个 vanilla display slot 的 `slot → objective name`。19 = 3 主槽
/// （list/sidebar/below_name）+ 16 个 team-color sidebar 变体。
#[derive(Component, Default, Debug, Clone)]
pub struct ScoreboardDisplay {
    map: HashMap<DisplaySlot, String>,
}

impl ScoreboardDisplay {
    pub fn slot(&self, slot: DisplaySlot) -> Option<&str> {
        self.map.get(&slot).map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&DisplaySlot, &str)> {
        self.map.iter().map(|(s, n)| (s, n.as_str()))
    }

    fn set(&mut self, slot: DisplaySlot, name: Option<String>) {
        match name {
            Some(n) => {
                self.map.insert(slot, n);
            }
            None => {
                self.map.remove(&slot);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Objective {
    /// vanilla `name`：scoreboard internal id（key）+ wire 字段。
    pub name: String,
    pub display_name: FormattedText,
    pub criteria: ObjectiveCriteria,
    pub number_format: NumberFormat,
}

/// **Per-LocalEntity** 的 objective 注册表。`name → Objective`，handler
/// `Add` / `Change` 写，`Remove` 删（并同步清 [`ScoreboardScores`] 里
/// 该 objective 的所有分数）。
#[derive(Component, Default, Debug, Clone)]
pub struct ScoreboardObjectives {
    map: HashMap<String, Objective>,
}

impl ScoreboardObjectives {
    pub fn get(&self, name: &str) -> Option<&Objective> {
        self.map.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Objective)> {
        self.map.iter()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// **Per-LocalEntity** 的 `(objective_name, holder) → i32`。holder 是 player
/// name 或 fake-player / entity uuid 字符串，按 vanilla 协议直接照搬。
#[derive(Component, Default, Debug, Clone)]
pub struct ScoreboardScores {
    map: HashMap<(String, String), i32>,
}

impl ScoreboardScores {
    pub fn get(&self, objective: &str, holder: &str) -> Option<i32> {
        self.map
            .get(&(objective.to_owned(), holder.to_owned()))
            .copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&(String, String), &i32)> {
        self.map.iter()
    }

    /// 列出指定 objective 下所有 (holder, score)，不保证顺序。UI 排序自己做。
    pub fn for_objective<'a>(
        &'a self,
        objective: &'a str,
    ) -> impl Iterator<Item = (&'a str, i32)> + 'a {
        self.map
            .iter()
            .filter(move |((obj, _), _)| obj == objective)
            .map(|((_, holder), s)| (holder.as_str(), *s))
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// scoreboard 任意一份字段变更通知（display slot / objective 注册 /
/// score）。下游 UI 想懒重绘 sidebar 监听这个就够。`entity` 是收到 packet
/// 的 local player——swarm 多 client 时下游可以按 client 过滤。
#[derive(Message, Clone, Debug)]
pub struct ScoreboardChanged {
    pub entity: Entity,
    pub kind: ScoreboardChangeKind,
}

#[derive(Clone, Debug)]
pub enum ScoreboardChangeKind {
    Display(DisplaySlot),
    Objective(String),
    Score { objective: String, holder: String },
}

/// 单包 `ClientboundSetDisplayObjective` 接入口。
pub fn apply_set_display_objective(
    ecs: &mut World,
    player: Entity,
    p: &ClientboundSetDisplayObjective,
) {
    let mut state = SystemState::<(
        Commands,
        Query<&mut ScoreboardDisplay>,
        MessageWriter<ScoreboardChanged>,
    )>::new(ecs);
    let (mut commands, mut query, mut writer) = state.get_mut(ecs);

    // vanilla：空字符串 = 取消该 slot 的显示。
    let name = if p.objective_name.is_empty() {
        None
    } else {
        Some(p.objective_name.clone())
    };
    if let Ok(mut display) = query.get_mut(player) {
        display.set(p.slot, name);
    } else {
        let mut new_display = ScoreboardDisplay::default();
        new_display.set(p.slot, name);
        commands.entity(player).insert(new_display);
    }
    writer.write(ScoreboardChanged {
        entity: player,
        kind: ScoreboardChangeKind::Display(p.slot),
    });
    state.apply(ecs);
}

/// 单包 `ClientboundSetObjective` 接入口。
pub fn apply_set_objective(ecs: &mut World, player: Entity, p: &ClientboundSetObjective) {
    let mut state = SystemState::<(
        Commands,
        Query<(
            Option<&mut ScoreboardObjectives>,
            Option<&mut ScoreboardScores>,
            Option<&mut ScoreboardDisplay>,
        )>,
        MessageWriter<ScoreboardChanged>,
    )>::new(ecs);
    let (mut commands, mut query, mut writer) = state.get_mut(ecs);

    let name = p.objective_name.clone();
    let entity_ref = query.get_mut(player).ok();
    let (mut objectives_opt, mut scores_opt, mut display_opt) = match entity_ref {
        Some(t) => t,
        None => (None, None, None),
    };

    match &p.method {
        Method::Add {
            display_name,
            render_type,
            number_format,
        }
        | Method::Change {
            display_name,
            render_type,
            number_format,
        } => {
            let new_obj = Objective {
                name: name.clone(),
                display_name: display_name.clone(),
                criteria: *render_type,
                number_format: number_format.clone(),
            };
            if let Some(objectives) = objectives_opt.as_deref_mut() {
                objectives.map.insert(name.clone(), new_obj);
            } else {
                let mut o = ScoreboardObjectives::default();
                o.map.insert(name.clone(), new_obj);
                commands.entity(player).insert(o);
            }
        }
        Method::Remove => {
            if let Some(objectives) = objectives_opt.as_deref_mut() {
                objectives.map.remove(&name);
            }
            // 同步清理 scores 表里所有该 objective 的条目，避免 stale 分数残留。
            if let Some(scores) = scores_opt.as_deref_mut() {
                scores.map.retain(|(obj, _), _| obj != &name);
            }
            // 任何 display slot 引用了它就清空。
            if let Some(display) = display_opt.as_deref_mut() {
                display.map.retain(|_, v| v != &name);
            }
        }
    }
    writer.write(ScoreboardChanged {
        entity: player,
        kind: ScoreboardChangeKind::Objective(name),
    });
    state.apply(ecs);
}

/// 单包 `ClientboundSetScore` 接入口。
pub fn apply_set_score(ecs: &mut World, player: Entity, p: &ClientboundSetScore) {
    let mut state = SystemState::<(
        Commands,
        Query<&mut ScoreboardScores>,
        MessageWriter<ScoreboardChanged>,
    )>::new(ecs);
    let (mut commands, mut query, mut writer) = state.get_mut(ecs);

    // protocol 把 score 编成 u32（其实是 i32 wire-cast）；下游用 i32 更顺手。
    let key = (p.objective_name.clone(), p.owner.clone());
    let val = p.score as i32;
    if let Ok(mut scores) = query.get_mut(player) {
        scores.map.insert(key, val);
    } else {
        let mut s = ScoreboardScores::default();
        s.map.insert(key, val);
        commands.entity(player).insert(s);
    }
    writer.write(ScoreboardChanged {
        entity: player,
        kind: ScoreboardChangeKind::Score {
            objective: p.objective_name.clone(),
            holder: p.owner.clone(),
        },
    });
    state.apply(ecs);
}

/// 单包 `ClientboundResetScore` 接入口。
pub fn apply_reset_score(ecs: &mut World, player: Entity, p: &ClientboundResetScore) {
    let mut state = SystemState::<(
        Query<&mut ScoreboardScores>,
        MessageWriter<ScoreboardChanged>,
    )>::new(ecs);
    let (mut query, mut writer) = state.get_mut(ecs);

    let kind = match &p.objective_name {
        Some(obj) => {
            if let Ok(mut scores) = query.get_mut(player) {
                scores.map.remove(&(obj.clone(), p.owner.clone()));
            }
            ScoreboardChangeKind::Score {
                objective: obj.clone(),
                holder: p.owner.clone(),
            }
        }
        None => {
            // vanilla `clearPlayerScores`：删该 holder 在所有 objective 下的分数。
            if let Ok(mut scores) = query.get_mut(player) {
                scores.map.retain(|(_, holder), _| holder != &p.owner);
            }
            ScoreboardChangeKind::Score {
                objective: String::new(),
                holder: p.owner.clone(),
            }
        }
    };
    writer.write(ScoreboardChanged {
        entity: player,
        kind,
    });
    state.apply(ecs);
}

#[cfg(test)]
mod tests {
    use bevy_app::App;

    use super::*;

    fn add_app() -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins(ScoreboardPlugin);
        let player = app.world_mut().spawn_empty().id();
        (app, player)
    }

    #[test]
    fn display_slot_set_and_clear() {
        let (mut app, player) = add_app();
        apply_set_display_objective(
            app.world_mut(),
            player,
            &ClientboundSetDisplayObjective {
                slot: DisplaySlot::Sidebar,
                objective_name: "deaths".to_owned(),
            },
        );
        // Commands 走的是 ECS deferred queue —— state.apply 已 flush，
        // 现在直接读 entity 即可。
        assert_eq!(
            app.world()
                .entity(player)
                .get::<ScoreboardDisplay>()
                .unwrap()
                .slot(DisplaySlot::Sidebar),
            Some("deaths"),
        );

        // 空字符串 = 清空该 slot
        apply_set_display_objective(
            app.world_mut(),
            player,
            &ClientboundSetDisplayObjective {
                slot: DisplaySlot::Sidebar,
                objective_name: String::new(),
            },
        );
        assert_eq!(
            app.world()
                .entity(player)
                .get::<ScoreboardDisplay>()
                .unwrap()
                .slot(DisplaySlot::Sidebar),
            None,
        );
    }

    #[test]
    fn team_color_sidebar_slots_kept_separate() {
        let (mut app, player) = add_app();
        apply_set_display_objective(
            app.world_mut(),
            player,
            &ClientboundSetDisplayObjective {
                slot: DisplaySlot::Sidebar,
                objective_name: "global".to_owned(),
            },
        );
        apply_set_display_objective(
            app.world_mut(),
            player,
            &ClientboundSetDisplayObjective {
                slot: DisplaySlot::TeamRed,
                objective_name: "red_only".to_owned(),
            },
        );
        let display = app
            .world()
            .entity(player)
            .get::<ScoreboardDisplay>()
            .unwrap();
        assert_eq!(display.slot(DisplaySlot::Sidebar), Some("global"));
        assert_eq!(display.slot(DisplaySlot::TeamRed), Some("red_only"));
        // BelowName / List 没设过 → None
        assert_eq!(display.slot(DisplaySlot::BelowName), None);
    }

    #[test]
    fn objective_add_change_remove_cleans_dependent_state() {
        let (mut app, player) = add_app();

        apply_set_objective(
            app.world_mut(),
            player,
            &ClientboundSetObjective {
                objective_name: "kills".to_owned(),
                method: Method::Add {
                    display_name: FormattedText::from("Kills".to_owned()),
                    render_type: ObjectiveCriteria::Integer,
                    number_format: NumberFormat::Blank,
                },
            },
        );
        apply_set_display_objective(
            app.world_mut(),
            player,
            &ClientboundSetDisplayObjective {
                slot: DisplaySlot::Sidebar,
                objective_name: "kills".to_owned(),
            },
        );
        apply_set_score(
            app.world_mut(),
            player,
            &ClientboundSetScore {
                owner: "alice".to_owned(),
                objective_name: "kills".to_owned(),
                score: 5,
                display: None,
                number_format: None,
            },
        );

        assert_eq!(
            app.world()
                .entity(player)
                .get::<ScoreboardScores>()
                .unwrap()
                .get("kills", "alice"),
            Some(5)
        );

        // Remove 同步清 display + scores
        apply_set_objective(
            app.world_mut(),
            player,
            &ClientboundSetObjective {
                objective_name: "kills".to_owned(),
                method: Method::Remove,
            },
        );
        let entity_ref = app.world().entity(player);
        assert!(
            entity_ref
                .get::<ScoreboardObjectives>()
                .unwrap()
                .is_empty()
        );
        assert!(entity_ref.get::<ScoreboardScores>().unwrap().is_empty());
        assert_eq!(
            entity_ref
                .get::<ScoreboardDisplay>()
                .unwrap()
                .slot(DisplaySlot::Sidebar),
            None,
        );
    }

    #[test]
    fn reset_score_with_objective_removes_one_entry() {
        let (mut app, player) = add_app();
        apply_set_score(
            app.world_mut(),
            player,
            &ClientboundSetScore {
                owner: "alice".to_owned(),
                objective_name: "kills".to_owned(),
                score: 5,
                display: None,
                number_format: None,
            },
        );
        apply_set_score(
            app.world_mut(),
            player,
            &ClientboundSetScore {
                owner: "alice".to_owned(),
                objective_name: "deaths".to_owned(),
                score: 2,
                display: None,
                number_format: None,
            },
        );
        apply_reset_score(
            app.world_mut(),
            player,
            &ClientboundResetScore {
                owner: "alice".to_owned(),
                objective_name: Some("kills".to_owned()),
            },
        );
        let scores = app
            .world()
            .entity(player)
            .get::<ScoreboardScores>()
            .unwrap();
        assert_eq!(scores.get("kills", "alice"), None);
        assert_eq!(scores.get("deaths", "alice"), Some(2));
    }

    #[test]
    fn reset_score_no_objective_removes_all_for_holder() {
        let (mut app, player) = add_app();
        apply_set_score(
            app.world_mut(),
            player,
            &ClientboundSetScore {
                owner: "bob".to_owned(),
                objective_name: "kills".to_owned(),
                score: 5,
                display: None,
                number_format: None,
            },
        );
        apply_set_score(
            app.world_mut(),
            player,
            &ClientboundSetScore {
                owner: "bob".to_owned(),
                objective_name: "deaths".to_owned(),
                score: 2,
                display: None,
                number_format: None,
            },
        );
        apply_set_score(
            app.world_mut(),
            player,
            &ClientboundSetScore {
                owner: "alice".to_owned(),
                objective_name: "kills".to_owned(),
                score: 1,
                display: None,
                number_format: None,
            },
        );
        apply_reset_score(
            app.world_mut(),
            player,
            &ClientboundResetScore {
                owner: "bob".to_owned(),
                objective_name: None,
            },
        );
        let scores = app
            .world()
            .entity(player)
            .get::<ScoreboardScores>()
            .unwrap();
        assert_eq!(scores.get("kills", "bob"), None);
        assert_eq!(scores.get("deaths", "bob"), None);
        assert_eq!(scores.get("kills", "alice"), Some(1));
    }

    #[test]
    fn two_local_players_isolated() {
        // azalea swarm: SetScore for p1 must not show up on p2.
        let mut app = App::new();
        app.add_plugins(ScoreboardPlugin);
        let p1 = app.world_mut().spawn_empty().id();
        let p2 = app.world_mut().spawn_empty().id();

        apply_set_score(
            app.world_mut(),
            p1,
            &ClientboundSetScore {
                owner: "alice".to_owned(),
                objective_name: "kills".to_owned(),
                score: 7,
                display: None,
                number_format: None,
            },
        );
        assert_eq!(
            app.world()
                .entity(p1)
                .get::<ScoreboardScores>()
                .unwrap()
                .get("kills", "alice"),
            Some(7)
        );
        assert!(app.world().entity(p2).get::<ScoreboardScores>().is_none());
    }
}
