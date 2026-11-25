//! The handoff a match's start takes.
//!
//! The player meets it in every match, so the switches it turns on are the
//! game's: every object's turn draws from its own random stream
//! ([`crate::sim::world::turn_streams`]) and the effect anims run as
//! presentation anims, as gamemd keeps them in Display and Logic.

use super::*;

#[test]
fn the_handoff_separates_the_turn_streams_and_presents_the_effect_anims() {
    let mut simulation = Simulation::new();
    assert!(!simulation.turn_streams_separate());
    assert!(!simulation.effect_anims_presented());
    simulation.begin_match_handoff();
    assert!(simulation.turn_streams_separate());
    assert!(simulation.effect_anims_presented());
}
