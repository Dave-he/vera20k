//! The trainer plays the game's match.
//!
//! The trainer measures a policy against the game the player meets, so its
//! match takes the handoff a real match's start takes
//! ([`crate::sim::world::Simulation::begin_match_handoff`]): every object's
//! turn draws from its own stream ([`crate::sim::world::turn_streams`]), not
//! from the shared one.

use super::*;

/// The match the trainer plays is not the shared-stream match: the draws
/// differ, so the trainer measures another game than it used to.
#[test]
#[ignore = "slow; loads a retail map"]
fn the_trainers_match_draws_differ_from_a_shared_stream_match() {
    let (retail, _) = crate::rules::retail_ini_fixture::retail_assets().expect("retail RA2_DIR");
    let mut request = AiMatchRequest::default();
    request.ticks = 120;

    let mut trained = load(&retail, &request).expect("the retail map loads");
    assert!(
        trained.runtime.simulation.turn_streams_separate(),
        "the trainer's match kept the shared stream"
    );
    run(&mut trained, &request).expect("the match plays");

    // The same match on the shared stream: the loader and the match are the
    // same, the draws are not.
    let mut shared =
        headless_scenario::load_session(&retail, &request.map, request.seed, session(&request))
            .expect("the retail map loads");
    shared.runtime.simulation.batch_path_searches();
    shared.runtime.simulation.batch_target_scans();
    shared.runtime.simulation.present_effect_anims();
    shared.runtime.simulation.sleep_idle_objects_in_match();
    run(&mut shared, &request).expect("the match plays");

    assert_ne!(
        trained.runtime.simulation.scenario_rng.state(),
        shared.runtime.simulation.scenario_rng.state(),
        "the trainer's match drew the shared stream's draws"
    );
}
