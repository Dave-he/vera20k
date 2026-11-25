//! The handoff a real match's start takes.
//!
//! A match does not start with a bare simulation: the searches and the
//! scans are batched, the effect anims run as presentation anims, every
//! object's turn draws from its own random stream
//! ([`crate::sim::world::turn_streams`]) and the idle objects sleep. A
//! player meets that match; so does every headless harness that plays the
//! game. The handoff is one owner, so a switch added to it reaches them
//! all.

use crate::sim::world::Simulation;

impl Simulation {
    /// Take the handoff a real match's start takes
    /// (`app::loading::transitions`).
    ///
    /// The batched searches and scans answer the same questions as the
    /// per-frame ones and cost less; the effect anims run as presentation
    /// anims, as gamemd keeps them in Display and Logic; the turn streams
    /// give every object its own random draws
    /// ([`Self::separate_turn_streams`]); and the idle objects sleep, which
    /// changes no result (`Self::sleep_idle_objects_in_match`).
    pub fn begin_match_handoff(&mut self) {
        self.batch_path_searches();
        self.batch_target_scans();
        self.present_effect_anims();
        self.separate_turn_streams(true);
        self.sleep_idle_objects_in_match();
    }
}

#[cfg(all(test, tests_sim_world_c_o))]
#[path = "match_handoff_tests.rs"]
mod tests;
