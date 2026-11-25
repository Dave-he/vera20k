//! VERA's own computer player: the tactics a house plays above the native
//! build queue.
//!
//! The native computer player (`sim::house_strategy`, `sim::ai_base_building`)
//! is the rule of the game: it is a faithful port of gamemd's player and it
//! plays the way gamemd plays. What is missing is not a bug but a decision
//! layer: the native player picks one enemy house, builds a crude queue and
//! sends everything it has after the greatest threat. A house that wants to
//! play well needs an army it assembles on purpose, an attack it commits to,
//! and a base it defends while it does.
//!
//! This module is that layer, and it plays only when a game's `[VeraAI]`
//! section turns it on (`rules::vera_ai::VeraAiPolicy::enabled`). With
//! retail data the section is absent, the policy is disabled and this module
//! does nothing, so the game plays as gamemd.
//!
//! The player keeps no state of its own. Every decision is read from the
//! world, so a save, a replay and a network peer agree without the policy
//! carrying anything the simulation does not already hold:
//!
//! - A combat object's squad is its stable id modulo the number of squads.
//!   A squad therefore keeps its members for as long as they live, without
//!   the player remembering who joined it.
//! - The category a house builds on a frame follows the frame number alone,
//!   so the rotation repeats without a counter.
//!
//! The orders are the same orders a player gives: an attack on a target and
//! an area guard, through the same owner a player's own orders use
//! ([`Simulation::apply_megamission_command`]). The rules of the game do not
//! change; only who decides what to do with them.

use crate::map::entities::EntityCategory;
use crate::rules::mission_data::MissionType;
use crate::rules::ruleset::RuleSet;
use crate::rules::vera_ai::VeraAiPolicy;
use crate::sim::ai_buildable::CountryBuildMasks;
use crate::sim::combat::TargetKind;
use crate::sim::command::MegaMissionTarget;
use crate::sim::intern::InternedId;
use crate::sim::production::ProductionCategory;
use crate::sim::world::{FrameEffects, Simulation};

/// The squads a house's army is split into. A squad's members are the
/// combat objects whose stable id falls in it.
const SQUAD_COUNT: u64 = 8;

/// One combat object of the house and what it is worth, for the squad order.
struct Combatant {
    id: u64,
    /// The type's strength: what the object is worth against the enemy.
    strength: i32,
}

/// Whether the type carries a weapon.
///
/// The type's weapon array slot 0 is gamemd's `Primary` field
/// (`TechnoTypeClass+0x898`), the weapon the type fires first; a type
/// without one (`Primary=none`, a harvester, a wall) carries nothing.
fn armed(object: &crate::rules::object_type::ObjectType) -> bool {
    object
        .primary()
        .is_some_and(|primary| !crate::rules::ini_parser::is_native_none_type_name(primary))
}

/// The house's living combat objects, strongest first.
///
/// An object is a combatant when it belongs to the house, lives, is not a
/// building, is not a civilian and carries a weapon. A harvester is a
/// vehicle without a weapon and is left to the native player.
fn combatants(sim: &Simulation, rules: &RuleSet, owner: InternedId) -> Vec<Combatant> {
    let mut list: Vec<Combatant> = Vec::new();
    for class in [
        EntityCategory::Infantry,
        EntityCategory::Unit,
        EntityCategory::Aircraft,
    ] {
        for &id in sim.substrate.entities.owned_ids(owner, class) {
            let Some(entity) = sim.substrate.entities.get(id) else {
                continue;
            };
            if !entity.is_alive() || entity.lifecycle.in_limbo() {
                continue;
            }
            let Some(object) = sim.interner.object_type(entity.type_ref(), rules) else {
                continue;
            };
            if object.civilian || !armed(object) {
                continue;
            }
            list.push(Combatant {
                id: entity.stable_id(),
                strength: object.strength,
            });
        }
    }
    list.sort_by(|left, right| {
        right
            .strength
            .cmp(&left.strength)
            .then_with(|| left.id.cmp(&right.id))
    });
    list
}

/// The house's enemy: the living house that is not its ally and stands
/// nearest to it.
///
/// The native player takes the first house in the house array, which in a
/// free-for-all is usually the human's; this takes the nearest, so an army
/// walks at the base that can be reached.
fn enemy_house(sim: &Simulation, owner: InternedId) -> Option<InternedId> {
    let home = sim.houses.get(&owner)?.base_center()?;
    let mut best: Option<(i64, InternedId)> = None;
    for candidate in sim.houses.order() {
        if *candidate == owner {
            continue;
        }
        let Some(house) = sim.houses.get(candidate) else {
            continue;
        };
        if house.is_defeated() || house.multiplay_passive() || house.is_human() {
            continue;
        }
        if sim
            .house_alliances
            .is_allied(&sim.interner, owner, *candidate)
        {
            continue;
        }
        let Some(center) = house.base_center() else {
            continue;
        };
        let dx = i64::from(center.0) - i64::from(home.0);
        let dy = i64::from(center.1) - i64::from(home.1);
        let distance = dx * dx + dy * dy;
        if best.is_none_or(|(best_distance, _)| distance < best_distance) {
            best = Some((distance, *candidate));
        }
    }
    best.map(|(_, house)| house)
}

/// The enemy's most valuable building: the one the army strikes.
///
/// The whole army goes for the one building: massing wins the fight, and a
/// building that falls takes the base's economy with it. When the building
/// is gone the next one takes its place.
fn priority_target(sim: &Simulation, rules: &RuleSet, enemy: InternedId) -> Option<u64> {
    let mut best: Option<(i32, u64)> = None;
    for &id in sim
        .substrate
        .entities
        .owned_ids(enemy, EntityCategory::Structure)
    {
        let Some(entity) = sim.substrate.entities.get(id) else {
            continue;
        };
        if !entity.is_alive() {
            continue;
        }
        let Some(object) = sim.interner.object_type(entity.type_ref(), rules) else {
            continue;
        };
        let name = sim.interner.resolve(entity.type_ref()).to_ascii_uppercase();
        let worth = target_worth(rules, &name, object);
        if best.is_none_or(|(best_worth, _)| worth > best_worth) {
            best = Some((worth, entity.stable_id()));
        }
    }
    best.map(|(_, id)| id)
}

/// What a building of the enemy is worth to strike.
///
/// The base units come first: a house that holds no base unit and no unit is
/// defeated (`HouseState::game_alive`), so the construction yard and the
/// factories are what keeps it in the game. A production building comes next,
/// and a defense after that: the strike takes the base's economy down before
/// the base's guns.
fn target_worth(
    rules: &RuleSet,
    name: &str,
    object: &crate::rules::object_type::ObjectType,
) -> i32 {
    if rules
        .general
        .base_unit_types
        .iter()
        .any(|base| base.eq_ignore_ascii_case(&name))
    {
        return 400;
    }
    if object.weapons_factory {
        300
    } else if object.anti_armor_value > 0 || object.anti_infantry_value > 0 {
        200
    } else {
        100
    }
}

/// The house's decision tick.
///
/// The house reads its own policy and plays by it. A house that no human
/// controls, in a game whose policy is enabled, decides every
/// `decision_period` frames.
pub(crate) fn update_vera_ai(
    sim: &mut Simulation,
    rules: &RuleSet,
    owner: InternedId,
    frame_effects: FrameEffects<'_>,
) {
    let policy = *rules.vera_ai();
    if !policy.enabled {
        return;
    }
    let Some(house) = sim.houses.get(&owner) else {
        return;
    };
    if house.is_controlled_by_human(sim.session.rule_policy()) || house.multiplay_passive() {
        return;
    }
    if !plays_vera_ai(sim, owner, policy) {
        return;
    }
    let frame = u64::from(sim.session.binary_frame());
    if frame % u64::from(policy.decision_period.max(1) as u32) != 0 {
        return;
    }
    let opening = frame < u64::from(policy.opening_delay.max(0) as u32);

    // The enemy: the house the army walks at.
    let enemy = if opening {
        None
    } else {
        enemy_house(sim, owner)
    };

    // The army: the guard that stays and the squads that leave.
    let army = combatants(sim, rules, owner);
    let home = sim.houses.get(&owner).and_then(|house| house.base_center());
    let waves = u64::from(policy.attack_waves.clamp(1, SQUAD_COUNT as i32) as u32);
    let threshold = policy.attack_threshold.max(0) as usize;
    let guard = (policy.home_guard.max(0) as usize).min(army.len());
    // The army is sorted strongest first, so the tail is the weakest: it
    // holds the base while the rest goes out.
    let split = army.len().saturating_sub(guard);
    let (attackers, guards) = army.split_at(split);
    let attackers: Vec<u64> = attackers.iter().map(|unit| unit.id).collect();
    let guard_ids: Vec<u64> = guards.iter().map(|unit| unit.id).collect();
    let ready = attackers.len() >= threshold;
    let target = enemy.and_then(|enemy| priority_target(sim, rules, enemy));

    for id in &guard_ids {
        if let Some(home) = home {
            order_guard(sim, *id, home, rules, frame_effects);
        }
    }
    if ready && let Some(target) = target {
        for id in &attackers {
            let squad = id % SQUAD_COUNT;
            if squad < waves {
                order_attack(sim, *id, target, rules, frame_effects);
            } else if let Some(home) = home {
                order_guard(sim, *id, home, rules, frame_effects);
            }
        }
    } else if let Some(home) = home {
        for id in &attackers {
            order_guard(sim, *id, home, rules, frame_effects);
        }
    }

    produce(sim, rules, owner, policy, frame, frame_effects);
}

/// Whether this house is one VERA's own player takes.
///
/// The policy's `max_houses` limits how many computer houses play by it, in
/// house order, so a match can pit a VERA house against a stock one.
fn plays_vera_ai(sim: &Simulation, owner: InternedId, policy: VeraAiPolicy) -> bool {
    if policy.max_houses <= 0 {
        return true;
    }
    let order = sim.houses.order();
    let Some(index) = order.iter().position(|id| *id == owner) else {
        return false;
    };
    let preceding = order[..index]
        .iter()
        .filter(|id| {
            sim.houses
                .get(id)
                .is_some_and(|house| !house.is_human() && !house.multiplay_passive())
        })
        .count();
    preceding < policy.max_houses as usize
}

/// Order one object onto the enemy's priority target.
///
/// The order is the one a player gives by clicking the target: the attack
/// event with the target object, through the same owner the player's own
/// clicks use. A unit that already holds this target keeps its order, so the
/// decision tick does not churn the army.
fn order_attack(
    sim: &mut Simulation,
    id: u64,
    target: u64,
    rules: &RuleSet,
    frame_effects: FrameEffects<'_>,
) {
    if holds_target(sim, id, target) {
        return;
    }
    sim.apply_megamission_command(
        id,
        MissionType::Attack,
        Some(TargetKind::Entity(target)),
        None,
        MegaMissionTarget::Null,
        rules,
        frame_effects,
    );
}

/// Whether the object already holds the target on an attack.
///
/// The stock player manages the same house's units: it puts them in teams
/// and its team scripts give them their own orders. A unit that is not on
/// the attack is given the order again, so the house's own player does not
/// take the army back.
fn holds_target(sim: &Simulation, id: u64, target: u64) -> bool {
    let Some(entity) = sim.substrate.entities.get(id) else {
        return false;
    };
    entity.attack_target.target() == Some(TargetKind::Entity(target))
        && entity.mission.current().known() == Some(MissionType::Attack)
}

/// Order one object to hold its ground at the base.
fn order_guard(
    sim: &mut Simulation,
    id: u64,
    cell: (u16, u16),
    rules: &RuleSet,
    frame_effects: FrameEffects<'_>,
) {
    sim.apply_megamission_command(
        id,
        MissionType::AreaGuard,
        Some(TargetKind::Cell(cell.0, cell.1)),
        None,
        MegaMissionTarget::Null,
        rules,
        frame_effects,
    );
}

/// The production the house does on this frame.
///
/// The category follows the frame alone, so the house's output keeps the
/// policy's proportions without a counter of its own.
fn produce(
    sim: &mut Simulation,
    rules: &RuleSet,
    owner: InternedId,
    policy: VeraAiPolicy,
    frame: u64,
    frame_effects: FrameEffects<'_>,
) {
    if !policy.target_production {
        return;
    }
    let categories = [
        (
            ProductionCategory::Infantry,
            policy.infantry_weight,
            policy.infantry_period,
        ),
        (
            ProductionCategory::Vehicle,
            policy.vehicle_weight,
            policy.vehicle_period,
        ),
        (
            ProductionCategory::Aircraft,
            policy.aircraft_weight,
            policy.aircraft_period,
        ),
    ];
    for (category, weight, period) in categories {
        if weight <= 0.0 || period <= 0 {
            continue;
        }
        let period = u64::from(period as u32);
        if frame % period != 0 {
            continue;
        }
        let Some(name) = next_type(sim, rules, owner, category, frame / period) else {
            continue;
        };
        let owner_name = sim.interner.resolve(owner).to_string();
        crate::sim::production::enqueue_by_type(
            sim,
            rules,
            &owner_name,
            &name,
            frame_effects,
            false,
        );
    }
}

/// The next type of a category the house may build.
///
/// The house's own buildable types of the category, in registry order, and
/// the index into them follows the count of the rounds the house has run, so
/// the house walks through what it may build.
fn next_type(
    sim: &Simulation,
    rules: &RuleSet,
    owner: InternedId,
    category: ProductionCategory,
    round: u64,
) -> Option<String> {
    let house = sim.houses.get(&owner)?;
    let country = sim.interner.resolve(house.country()?).to_string();
    let masks = CountryBuildMasks::for_country(rules, &country);
    let super_weapons = sim.session.game_options().super_weapons;
    let object_category = category.object_category();
    let mut candidates: Vec<String> = Vec::new();
    for (index, id) in rules.type_array_ids(object_category).iter().enumerate() {
        let Some(object) = rules.type_array_at(object_category, index as i32) else {
            continue;
        };
        if !crate::sim::ai_buildable::candidate_allowed(
            object,
            masks,
            house.side_index(),
            super_weapons,
            rules,
        ) {
            continue;
        }
        candidates.push(id.clone());
    }
    if candidates.is_empty() {
        return None;
    }
    let index = (round as usize) % candidates.len();
    candidates.get(index).cloned()
}

#[cfg(all(test, tests_sim_t_z))]
#[path = "vera_ai_tests.rs"]
mod tests;
