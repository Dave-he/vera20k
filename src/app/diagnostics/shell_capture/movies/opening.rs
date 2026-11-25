//! The existing campaign capture route continues through the actual app
//! runtime. Timer captures explicitly replay a later mission's parsed timer
//! actions after opening unlock; they do not claim those objectives were played.

use super::*;
use crate::app::input::{camera, hotkeys, keyboard};
use crate::app::match_runtime::sim_host::SimWrite;
use crate::app::match_runtime::sim_tick::{ExactStepError, ExactStepReceipt};
use crate::sim::intern::InternedId;
use crate::sim::mission::{MissionId, MissionType};
use crate::sim::world::Simulation;
use winit::event::{ElementState, MouseButton};
use winit::keyboard::{Key, KeyCode, KeyLocation, NamedKey};

mod aircraft;

/// The cells a trigger trace may name, in capture order.
pub type TraceCaptureCells = [(u16, u16); MAX_TRACE_CAPTURE_CELLS];

/// How many capture cells a trigger trace may name.
pub const MAX_TRACE_CAPTURE_CELLS: usize = 8;

/// The legs a trigger trace may name: the tick each starts on and its cell.
pub type TraceLegs = [(u64, (u16, u16)); MAX_TRACE_LEGS];

/// How many legs a trigger trace may name.
pub const MAX_TRACE_LEGS: usize = 8;

/// How many bytes a trained unit type name may hold.
pub const TRACE_TYPE_NAME_MAX: usize = 16;

/// How many unit types a trigger trace may train.
pub const MAX_TRACE_TRAIN: usize = 4;

/// A unit type name a trigger trace trains. The name is held inline, so the
/// whole trace stays `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TraceTypeName {
    bytes: [u8; TRACE_TYPE_NAME_MAX],
    len: u8,
}

impl TraceTypeName {
    /// Hold the name, if it fits.
    pub fn new(name: &str) -> Option<Self> {
        let bytes = name.as_bytes();
        if bytes.len() > TRACE_TYPE_NAME_MAX {
            return None;
        }
        let mut held = [0u8; TRACE_TYPE_NAME_MAX];
        held[..bytes.len()].copy_from_slice(bytes);
        Some(Self {
            bytes: held,
            len: bytes.len() as u8,
        })
    }

    /// The name as the type table spells it.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }
}

/// The units a trigger trace trains: each type name and how many of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TraceTrain {
    entries: [(TraceTypeName, u32); MAX_TRACE_TRAIN],
    count: u8,
}

impl TraceTrain {
    /// The train orders, in the order they were named.
    pub fn iter<'a>(&'a self) -> impl Iterator<Item = (&'a str, u32)> + 'a {
        self.entries[..self.count as usize]
            .iter()
            .map(|(name, count)| (name.as_str(), *count))
    }

    /// Name one more type and how many of it, if the list has room.
    pub fn push(&mut self, name: TraceTypeName, count: u32) -> bool {
        let next = self.count as usize;
        if next >= MAX_TRACE_TRAIN {
            return false;
        }
        self.entries[next] = (name, count);
        self.count += 1;
        true
    }

    /// How many types the list names.
    pub fn count(&self) -> usize {
        self.count as usize
    }
}

/// How long a capture order may stay without reaching its engineer before
/// the probe treats it as dropped and frees the cell for the next building.
const CAPTURE_ORDER_GRACE_TICKS: u64 = 120;

/// The player orders a trigger trace plays.
///
/// `push` sends the player's armed units toward a cell. `capture` names the
/// buildings the probe captures, in order, as engineers become available.
/// `legs` names the cells the units attack toward in turn, each from its own
/// tick. `train` names the units the player trains. `capture_after` is the
/// tick the capture orders may first be issued on. `film_every` is the tick
/// interval the retail screenshot key is pressed on, so the run leaves the
/// pictures a player would take of the same battle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TraceOrders {
    /// The cell the armed force attacks toward.
    pub push: Option<(u16, u16)>,
    /// The buildings the probe captures, in the order they were named.
    pub capture: Option<(TraceCaptureCells, u8)>,
    /// The cells the force attacks toward in turn, each with its own tick.
    pub legs: Option<(TraceLegs, u8)>,
    /// The unit types the player trains and how many of each.
    pub train: TraceTrain,
    /// The tick the capture orders may first be issued on.
    pub capture_after: Option<u64>,
    /// The tick interval the retail screenshot key is pressed on.
    pub film_every: Option<u64>,
}

/// One accepted capture order and how it ended.
struct CaptureOrder {
    /// The tick the order was issued on.
    tick: u64,
    /// The engineer that carries the order.
    engineer: u64,
    /// The building the engineer was sent to capture.
    target: u64,
    /// The cell the building stands on.
    cell: (u16, u16),
    /// The tick the building changed hands.
    completed: Option<u64>,
    /// Where the engineer stood when the order was issued.
    start_cell: (u16, u16),
    /// Where the engineer stood the last time it was seen.
    last_cell: (u16, u16),
    /// The cells the engineer walked through, with the tick it reached them.
    route: Vec<(u64, (u16, u16))>,
    /// Why the order ended without the building changing hands.
    dropped: Option<&'static str>,
    /// The engineer's health each time it changed, with the tick it changed.
    health: Vec<(u64, i32)>,
    /// The hostile units next to the engineer when its health last changed.
    threats: Vec<String>,
}

/// The probe runs one capture at a time, as a player does. Keep progress
/// attached to the individual attempt even if another order uses the same
/// engineer or cell.
#[derive(Clone, Copy)]
struct CaptureInFlight {
    order: usize,
    carrying: bool,
}

/// An engineer the map is not using: it carries no order of its own. A
/// guarding engineer waits for orders; a scripted one does not.
fn engineer_is_free(current: MissionId, queued: MissionId, suspended: MissionId) -> bool {
    (current == MissionId::NONE || current.known() == Some(MissionType::Guard))
        && queued == MissionId::NONE
        && suspended == MissionId::NONE
}

/// The next building the probe captures: the first named cell that holds a
/// building the player does not own yet. The capture list is worked in the
/// order it was named, so a building whose capture is still running keeps
/// its place and the probe waits for it.
fn next_capture_cell(
    cells: &[(u16, u16)],
    mut building_at: impl FnMut((u16, u16)) -> Option<(u64, bool)>,
) -> Option<(u64, (u16, u16))> {
    for cell in cells {
        if let Some((id, owned)) = building_at(*cell)
            && !owned
        {
            return Some((id, *cell));
        }
    }
    None
}

/// The hostile units next to the watched engineer.
///
/// A hostile unit more than three cells away cannot have reached the
/// engineer in one step, so the list keeps what a bullet or a turret could
/// have hit. The engineer itself never appears in it.
fn select_threats(
    engineer: u64,
    engineer_cell: (u16, u16),
    candidates: &[(u64, String, (u16, u16), bool)],
) -> Vec<String> {
    let mut threats = Vec::new();
    for (id, type_name, cell, targeting) in candidates {
        if *id == engineer {
            continue;
        }
        let distance = cell
            .0
            .abs_diff(engineer_cell.0)
            .max(cell.1.abs_diff(engineer_cell.1));
        if distance > 3 {
            continue;
        }
        let mut line = format!("{type_name}#{id} at ({},{})", cell.0, cell.1);
        if *targeting {
            line.push_str(" targets the engineer");
        }
        threats.push(line);
        if threats.len() == 8 {
            break;
        }
    }
    threats
}

/// The hostile units next to the watched engineer, read from the world.
fn threats_near(
    sim: &Simulation,
    player: InternedId,
    engineer: u64,
    cell: (u16, u16),
) -> Vec<String> {
    let candidates = sim
        .entities()
        .iter_sorted()
        .filter(|(_, entity)| entity.owner() != player && entity.is_ai_alive())
        .map(|(id, entity)| {
            let targeting = matches!(
                entity.attack_target.target(),
                Some(crate::sim::combat::TargetKind::Entity(target)) if target == engineer
            );
            (
                id,
                sim.interner().resolve(entity.type_ref()).to_owned(),
                (entity.position.rx(), entity.position.ry()),
                targeting,
            )
        })
        .collect::<Vec<_>>();
    select_threats(engineer, cell, &candidates)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpeningTarget {
    Locked,
    CameraPan,
    Flash,
    Arrows,
    Unlocked,
    /// Let the normal wall-clock pacer and audio device run through SOV01.
    RealtimeUnlocked,
    RestoreUnlocked,
    /// ALL01, whose first authored action locks input: an unlocked save
    /// loaded from the Esc menu while a replay of the opening holds the lock.
    LoadUnlockedWhileLocked,
    /// A fixed committed production tick, for opening aircraft raster diagnosis.
    AlliedTick(u64),
    /// Observe the ordinary SOV01 simulation after its opening unlock.
    SovietTick(u64),
    /// Issue ordinary naval attack orders, then observe the committed tick.
    SovietNavy(u64),
    /// Stock SOV07: ordinary player attack-move toward the NE setpiece.
    SovietGroups(u64),
    /// Stock SOV02: ordinary player attack-move toward the Battle Fortresses.
    SovietFortresses(u64),
    /// SOV01 timer actions replayed after opening unlock, then ordinary expiry.
    TimerRunning,
    TimerExpired,
    /// SOV02's ordinary briefing Action55 creates a type-5 alert.
    RadarAlert,
    /// Any retail campaign mission run to a committed tick, recording every
    /// trigger Spring on the way, plus what a native replay of the trigger
    /// classes needs: the starting Tags, the poll and House-raise bounds,
    /// world raises, world-reading event results, attachment counts,
    /// variable writes and run-time Tags. Without `push` there is no player
    /// input unless a probe is requested. The `orders` are the player's
    /// orders the trace plays: `push` sends the player's armed units
    /// toward a cell, `capture` names the buildings the probe captures, in
    /// order, as engineers become available, `legs` names the cells the
    /// units attack toward in turn, `train` names the units the player
    /// trains and `capture_after` is the tick the captures may start on.
    TriggerTrace {
        scenario: &'static str,
        tick: u64,
        orders: TraceOrders,
    },
    /// Save/load that naturally created alert, then present the restored radar.
    RadarRestored,
}

impl OpeningTarget {
    pub(super) fn phase(self) -> &'static str {
        match self {
            Self::Locked => "opening-locked",
            Self::CameraPan => "opening-pan",
            Self::Flash => "opening-flash",
            Self::Arrows => "opening-arrows",
            Self::Unlocked => "opening-unlocked",
            Self::RealtimeUnlocked => "opening-realtime-unlocked",
            Self::RestoreUnlocked => "opening-restore-unlocked",
            Self::LoadUnlockedWhileLocked => "opening-load-unlocked-while-locked",
            Self::AlliedTick(_) => "opening-allied-tick",
            Self::SovietTick(_) => "opening-soviet-tick",
            Self::SovietNavy(_) => "soviet-naval-encounter",
            Self::SovietGroups(_) => "sov07-group-encounter",
            Self::SovietFortresses(_) => "sov02-fortress-encounter",
            Self::TimerRunning => "opening-timer-running",
            Self::TimerExpired => "opening-timer-expired",
            Self::RadarAlert => "radar-alert",
            Self::RadarRestored => "radar-restored",
            Self::TriggerTrace { .. } => "trigger-trace",
        }
    }

    pub(crate) fn side(self) -> CampaignSide {
        match self {
            Self::LoadUnlockedWhileLocked | Self::AlliedTick(_) => CampaignSide::Allied,
            Self::TriggerTrace { scenario, .. } if scenario.starts_with("ALL") => {
                CampaignSide::Allied
            }
            _ => CampaignSide::Soviet,
        }
    }

    fn step_limit(self) -> usize {
        match self {
            Self::LoadUnlockedWhileLocked => 6_000,
            Self::SovietTick(tick)
            | Self::SovietNavy(tick)
            | Self::SovietGroups(tick)
            | Self::SovietFortresses(tick)
            | Self::TriggerTrace { tick, .. } => tick as usize + 1,
            Self::TimerRunning | Self::TimerExpired => 8_000,
            _ => 2_500,
        }
    }
}

/// Production steps run after the in-lock load before the readback.
const STEPS_AFTER_LOAD: u32 = 45;

pub(super) struct OpeningCapture {
    target: OpeningTarget,
    save_directory: PathBuf,
    initial_runtime: Option<Value>,
    steps: Vec<ExactStepReceipt>,
    observations: Vec<Value>,
    previous_projection: Option<Value>,
    saw_lock: bool,
    saw_unlock: bool,
    saw_countdown: bool,
    last_running_countdown: Option<Value>,
    timer_action_replay: Option<Value>,
    lock_probe: Option<Value>,
    unlock_probe: Option<Value>,
    locked_mouse_probe: Option<Value>,
    unlocked_mouse_probe: Option<Value>,
    restored: Option<Value>,
    /// LoadUnlockedWhileLocked: the save made before the first lock, the save
    /// made after its unlock, and the Esc-menu load made in the replayed lock.
    pre_lock_save: Option<PathBuf>,
    unlocked_save: Option<PathBuf>,
    load_while_locked: Option<Value>,
    steps_after_load: u32,
    /// The (tick, binary frame) a load installed, until the next step.
    loaded_frame: Option<(u64, u32)>,
    saved_radar: Option<crate::sim::snapshot::saved_local_view::SavedRadarEvents>,
    group_orders: Option<Value>,
    /// Trigger-trace push orders issued: tick and unit count of each.
    push_orders: Vec<(u64, usize)>,
    /// The cell the last push order named, so a new leg is sent at once.
    push_destination: Option<(u16, u16)>,
    /// Accepted captures and their observed results, in issue order.
    capture_orders: Vec<CaptureOrder>,
    capture_in_flight: Vec<CaptureInFlight>,
    /// The units the plan trains and how many of each.
    train: Vec<(String, u32)>,
    /// The tick the capture orders may first be issued on.
    capture_after: Option<u64>,
    /// The tick interval the retail screenshot key is pressed on.
    film_every: Option<u64>,
    /// The ticks the screenshot key was pressed on, in order.
    film_ticks: Vec<u64>,
    /// What the capture probe last saw: the named cells, their buildings,
    /// the engineer it picked and whether the order was accepted.
    capture_probe: Option<Value>,
    /// What the probe saw each time the capture state changed: the engineer
    /// it uses, where it stands, what it is doing and who owns the
    /// building. The list is the probe's record of the capture's progress.
    capture_log: Vec<Value>,
    trigger_springs: Vec<Value>,
    /// Poll and House-raise bounds, variable writes and run-time Tags.
    trigger_events: Vec<Value>,
    /// The trace's terminal outcome frame: a mission whose result was
    /// admitted inside the traced window admitted it on a frame native does
    /// not commit. The counters are the last committed ones; `admitting_tick`
    /// is the frame that admitted the result.
    outcome_frame: Option<Value>,
}

impl OpeningCapture {
    pub(super) fn new(target: OpeningTarget, save_directory: PathBuf) -> Self {
        // The train order is played once, so the capture owns it from here on.
        let mut train = Vec::new();
        let mut capture_after = None;
        let mut film_every = None;
        if let OpeningTarget::TriggerTrace { orders, .. } = &target {
            train = orders
                .train
                .iter()
                .map(|(name, count)| (name.to_string(), count))
                .collect();
            capture_after = orders.capture_after;
            film_every = orders.film_every;
        }
        Self {
            target,
            save_directory,
            initial_runtime: None,
            steps: Vec::new(),
            observations: Vec::new(),
            previous_projection: None,
            saw_lock: false,
            saw_unlock: false,
            saw_countdown: false,
            last_running_countdown: None,
            timer_action_replay: None,
            lock_probe: None,
            unlock_probe: None,
            locked_mouse_probe: None,
            unlocked_mouse_probe: None,
            restored: None,
            pre_lock_save: None,
            unlocked_save: None,
            load_while_locked: None,
            steps_after_load: 0,
            loaded_frame: None,
            saved_radar: None,
            group_orders: None,
            push_orders: Vec::new(),
            push_destination: None,
            capture_orders: Vec::new(),
            capture_in_flight: Vec::new(),
            capture_after,
            film_every,
            film_ticks: Vec::new(),
            train,
            capture_probe: None,
            capture_log: Vec::new(),
            trigger_springs: Vec::new(),
            trigger_events: Vec::new(),
            outcome_frame: None,
        }
    }

    pub(super) fn timeout(&self) -> Duration {
        Duration::from_secs(match self.target {
            OpeningTarget::LoadUnlockedWhileLocked
            | OpeningTarget::SovietTick(_)
            | OpeningTarget::SovietNavy(_)
            | OpeningTarget::SovietGroups(_)
            | OpeningTarget::SovietFortresses(_) => 600,
            OpeningTarget::TimerRunning | OpeningTarget::TimerExpired => 600,
            OpeningTarget::TriggerTrace { .. } => 3600,
            _ => 180,
        })
    }

    /// A trace whose mission ended is finished: the app leaves the in-game
    /// route for the victory tail, so the capture must not demand that route
    /// any more.
    pub(super) fn mission_ended(&self, state: &AppState) -> bool {
        if !matches!(self.target, OpeningTarget::TriggerTrace { .. }) {
            return false;
        }
        trace_outcome(state).is_some()
    }

    pub(super) fn ready(&self, state: &AppState) -> Result<bool> {
        if matches!(
            self.target,
            OpeningTarget::RadarAlert | OpeningTarget::RadarRestored
        ) {
            let Some(expected) = self.saved_radar.as_ref() else {
                return Ok(false);
            };
            let radar = state
                .match_state
                .match_presentation
                .minimap
                .as_ref()
                .context("restored radar")?;
            let actual = radar.saved_radar_events();
            // after_present may have just resumed the first gameplay frame
            // after load. Wait for its minimap draw before comparing it.
            if actual.last_advanced_frame < expected.last_advanced_frame {
                return Ok(false);
            }
            ensure!(
                actual == *expected,
                "first radar redraw changed the retained alert state: actual={actual:?}, expected={expected:?}"
            );
            let visible = state
                .match_state
                .match_presentation
                .radar_anim
                .as_ref()
                .is_none_or(|radar| radar.is_minimap_visible());
            return Ok(
                visible && (self.target == OpeningTarget::RadarAlert || self.restored.is_some())
            );
        }
        let sim = &state
            .match_state
            .sim_runtime
            .runtime()
            .context("campaign opening has no installed runtime")?
            .simulation;
        if let OpeningTarget::TriggerTrace { tick, .. } = self.target {
            // A mission that ends first stops the trace on its outcome frame.
            return Ok(trace_outcome(state).is_some() || sim.session().tick() == tick);
        }
        let locked = sim.session().input_locked();
        let view = camera::saved_tactical_view(
            &state.platform,
            &state.renderer,
            &state.frontend.screen,
            &state.match_state,
            &state.persistence,
        );
        Ok(self.saw_lock
            && match self.target {
                OpeningTarget::Locked => locked,
                OpeningTarget::CameraPan => mid_pan(&view),
                OpeningTarget::Flash => sim
                    .entities()
                    .iter_sorted()
                    .any(|(_, entity)| entity.is_active() && entity.flashing.flashed()),
                OpeningTarget::Arrows => sim.anims().any(|(_, anim)| anim.no_effects()),
                OpeningTarget::Unlocked | OpeningTarget::RealtimeUnlocked => {
                    self.saw_unlock && !locked
                }
                OpeningTarget::RestoreUnlocked => {
                    self.saw_unlock && !locked && self.restored.is_some()
                }
                OpeningTarget::LoadUnlockedWhileLocked => {
                    self.load_while_locked.is_some() && self.steps_after_load >= STEPS_AFTER_LOAD
                }
                OpeningTarget::AlliedTick(tick)
                | OpeningTarget::SovietTick(tick)
                | OpeningTarget::SovietNavy(tick)
                | OpeningTarget::SovietGroups(tick)
                | OpeningTarget::SovietFortresses(tick) => sim.session().tick() == tick,
                OpeningTarget::TimerRunning => sim.session().scenario_timer_view().is_some(),
                OpeningTarget::TimerExpired => {
                    self.saw_countdown && sim.session().scenario_timer_view().is_none()
                }
                OpeningTarget::RadarAlert
                | OpeningTarget::RadarRestored
                | OpeningTarget::TriggerTrace { .. } => unreachable!(),
            })
    }

    pub(super) fn after_present(&mut self, state: &mut AppState) -> Result<()> {
        if self.initial_runtime.is_none() {
            if self.target == OpeningTarget::RealtimeUnlocked {
                ensure!(
                    state.audio.sfx_player.is_some(),
                    "realtime opening requires audio output"
                );
            }
            self.initial_runtime = Some(if self.target == OpeningTarget::RealtimeUnlocked {
                installed_campaign_runtime_receipt(state)?
            } else {
                campaign_runtime_receipt(state)?
            });
            if let OpeningTarget::TriggerTrace { .. } = self.target {
                #[cfg(feature = "dev-overlays")]
                state
                    .match_state
                    .sim_runtime
                    .write_sim(SimWrite::Debug)
                    .context("trigger trace has no installed runtime")?
                    .simulation
                    .record_trigger_springs(true);
                #[cfg(not(feature = "dev-overlays"))]
                bail!("trigger traces need dev-overlays");
            }
            // A real pointer press before the first authored lock. Action46
            // must cancel its capture while preserving physical button state.
            if !matches!(
                self.target,
                OpeningTarget::RadarAlert
                    | OpeningTarget::RadarRestored
                    | OpeningTarget::TriggerTrace { .. }
            ) {
                crate::app::input::dispatch::handle_mouse_input(
                    state,
                    MouseButton::Left,
                    ElementState::Pressed,
                );
            }
        }
        self.observe(state)?;
        if let OpeningTarget::TriggerTrace {
            orders: TraceOrders {
                capture: Some(_), ..
            },
            ..
        } = self.target
        {
            // The capture orders are followed on every frame, the last one
            // included: the readback below returns before the capture block
            // runs, and a capture that landed on the final frame still has
            // to be in the receipt.
            self.note_capture_progress(state);
        }
        if matches!(
            self.target,
            OpeningTarget::RadarAlert | OpeningTarget::RadarRestored
        ) {
            self.capture_radar_load(state)?;
            if self.saved_radar.is_some() {
                return Ok(());
            }
        }
        let locked = crate::app::input::campaign_input::is_locked(&state.match_state);
        let probes = !matches!(
            self.target,
            OpeningTarget::RadarAlert
                | OpeningTarget::RadarRestored
                | OpeningTarget::TriggerTrace { .. }
        );
        if probes && locked && self.lock_probe.is_none() {
            self.lock_probe = Some(probe_arrow_edge(state, true)?);
            self.locked_mouse_probe = Some(probe_locked_mouse_edges(state)?);
        } else if probes && self.saw_unlock && !locked && self.unlock_probe.is_none() {
            self.unlock_probe = Some(probe_arrow_edge(state, false)?);
            self.unlocked_mouse_probe = Some(probe_unlocked_mouse_edges(state)?);
        }
        // Readback is submitted before this hook. Keep its receipt on the
        // exact state that was drawn, including the first unlocked frame.
        if self.ready(state)? {
            return Ok(());
        }
        // The screenshot request is consumed on the next render. A terminal
        // trace already has its own readback and will exit before that render.
        if let OpeningTarget::TriggerTrace { .. } = self.target {
            self.request_film_frames(state)?;
        }
        if self.target == OpeningTarget::RealtimeUnlocked {
            return Ok(());
        }
        if matches!(self.target, OpeningTarget::SovietNavy(_)) && self.saw_unlock && !locked {
            self.issue_naval_orders(state)?;
        }
        if matches!(
            self.target,
            OpeningTarget::SovietGroups(_) | OpeningTarget::SovietFortresses(_)
        ) && !locked
        {
            self.issue_campaign_encounter_orders(state)?;
        }
        // The first simulation tick can publish the briefing's input lock.
        // Let it commit before the trace sends any player orders.
        let capture = match &self.target {
            OpeningTarget::TriggerTrace { orders, .. } => orders.capture,
            _ => None,
        };
        if let OpeningTarget::TriggerTrace { .. } = self.target
            && !locked
            && state
                .match_state
                .sim_runtime
                .runtime()
                .is_some_and(|runtime| runtime.simulation.session().tick() > 0)
        {
            self.issue_trace_push_orders(state)?;
            if !self.train.is_empty() {
                for (type_id, count) in std::mem::take(&mut self.train) {
                    crate::app::input::commands::queue_build_by_type(state, &type_id, count);
                }
            }
            if let Some((cells, len)) = capture {
                self.issue_trace_capture(state, &cells[..len as usize])?;
            }
        }
        if matches!(
            self.target,
            OpeningTarget::TimerRunning | OpeningTarget::TimerExpired
        ) && self.saw_unlock
            && !locked
            && self.timer_action_replay.is_none()
        {
            self.replay_timer_actions(state)?;
            self.observe(state)?;
        }
        if self.target == OpeningTarget::RestoreUnlocked
            && self.restored.is_none()
            && mid_pan(&camera::saved_tactical_view(
                &state.platform,
                &state.renderer,
                &state.frontend.screen,
                &state.match_state,
                &state.persistence,
            ))
        {
            state
                .persistence
                .isolate_capture_saves(&self.save_directory)
                .context("create isolated campaign capture save directory")?;
            let before = camera::saved_tactical_view(
                &state.platform,
                &state.renderer,
                &state.frontend.screen,
                &state.match_state,
                &state.persistence,
            );
            let depth = state.match_state.input.cursor_visibility.level();
            let saved_runtime = installed_campaign_runtime_receipt(state)?;
            let path =
                crate::app::persistence::commands::save_game(state, "SOV01 opening pan", None)
                    .context("save through ordinary production command")?;
            let saved_bytes = std::fs::read(&path).context("read saved capture bytes")?;
            crate::app::persistence::commands::try_load_save_file(state, &path)
                .context("load through ordinary prepared-load/commit owners")?;
            let after = camera::saved_tactical_view(
                &state.platform,
                &state.renderer,
                &state.frontend.screen,
                &state.match_state,
                &state.persistence,
            );
            ensure!(
                before == after,
                "ordinary load changed the retained pan trajectory"
            );
            ensure!(
                state.match_state.input.cursor_visibility.level() == depth,
                "ordinary load changed outgoing cursor display depth"
            );
            let loaded_runtime = installed_campaign_runtime_receipt(state)?;
            ensure!(
                saved_runtime["tick"] == loaded_runtime["tick"]
                    && saved_runtime["binary_frame"] == loaded_runtime["binary_frame"],
                "ordinary load changed the saved simulation frame"
            );
            self.restored = Some(json!({
                "path": path, "byte_length": saved_bytes.len(),
                "sha256": crate::util::sha256::sha256_hex(&saved_bytes),
                "view_before": before, "view_after": after,
                "cursor_depth_before": depth,
                "cursor_depth_after": state.match_state.input.cursor_visibility.level(),
                "saved_runtime": saved_runtime, "loaded_runtime": loaded_runtime,
                "rng_limit": "LoadScenario resets Scenario RNG; Main and MapGen are retained by their existing owners",
                "format": "VERA snapshot; native SAV wire compatibility is not claimed",
            }));
            self.observe(state)?;
        }
        if self.target == OpeningTarget::LoadUnlockedWhileLocked {
            self.drive_load_while_locked(state)?;
        }
        let limit = self.target.step_limit();
        ensure!(
            self.steps.len() < limit,
            "campaign opening did not reach {:?} within {limit} production steps",
            self.target
        );
        let receipt = match crate::app::match_runtime::sim_tick::advance_in_game_runtime_exact_step(
            state,
        ) {
            Ok(receipt) => receipt,
            Err(error) => {
                // A mission that ends inside the traced window admits its
                // result on a frame native does not commit: Main_Tick
                // (0x0055DE4F..0x0055DEC8) skips commit and deferred
                // destruction while the outcome waits. The trace stops on
                // that outcome frame instead of failing the step.
                let stops = state
                    .match_state
                    .sim_runtime
                    .runtime()
                    .map(|runtime| {
                        let sim = &runtime.simulation;
                        trace_stops_on_outcome_frame(
                            &self.target,
                            &error,
                            sim,
                            sim.session().current_house(),
                        )
                    })
                    .unwrap_or(false);
                if !stops {
                    log::error!(
                        "trigger trace step failed: error={error} outcome={:?} pending_wait={:?}",
                        trace_outcome(state),
                        state
                            .match_state
                            .sim_runtime
                            .runtime()
                            .map(|runtime| runtime.simulation.pending_outcome_wait())
                    );
                    return Err(error).context("advance exact ordinary campaign production frame");
                }
                let (tick, binary_frame) = installed_frame(state)?;
                self.collect_trigger_trace(state)?;
                self.observe(state)?;
                self.outcome_frame = Some(json!({
                    "outcome": trace_outcome(state),
                    "tick": tick,
                    "binary_frame": binary_frame,
                    // The frame that admitted the result never commits: the
                    // session's counters still show the frame before it.
                    "admitting_tick": tick.saturating_add(1),
                    "stop": "the mission's result was admitted inside the traced window",
                }));
                return Ok(());
            }
        };
        // A load installs another frame; the next step starts exactly there.
        let expected = self.loaded_frame.take().or_else(|| {
            self.steps
                .last()
                .map(|previous| (previous.tick_after, previous.binary_frame_after))
        });
        if let Some((tick, binary_frame)) = expected {
            ensure!(
                tick == receipt.tick_before && binary_frame == receipt.binary_frame_before,
                "campaign capture lost its contiguous runtime frame sequence"
            );
        }
        self.steps.push(receipt);
        self.collect_trigger_trace(state)?;
        self.observe(state)?;
        Ok(())
    }

    /// Take the trigger trace the runtime recorded since the last collection.
    #[cfg(feature = "dev-overlays")]
    fn collect_trigger_trace(&mut self, state: &mut AppState) -> Result<()> {
        if !matches!(self.target, OpeningTarget::TriggerTrace { .. }) {
            return Ok(());
        }
        let sim = &mut state
            .match_state
            .sim_runtime
            .write_sim(SimWrite::Debug)
            .context("trigger trace lost its runtime")?
            .simulation;
        let (springs, events) = sim.take_trigger_trace();
        self.trigger_springs
            .extend(springs.into_iter().map(|spring| json!(spring)));
        self.trigger_events
            .extend(events.into_iter().map(|event| json!(event)));
        Ok(())
    }

    /// Without the overlay the runtime records no trigger trace at all, and
    /// the capture has already refused the trigger trace itself.
    #[cfg(not(feature = "dev-overlays"))]
    fn collect_trigger_trace(&self, _state: &mut AppState) -> Result<()> {
        Ok(())
    }

    fn capture_radar_load(&mut self, state: &mut AppState) -> Result<()> {
        if self.saved_radar.is_some() {
            return Ok(());
        }
        let Some(minimap) = state.match_state.match_presentation.minimap.as_ref() else {
            return Ok(());
        };
        let before = minimap.saved_radar_events();
        // SOV02's ordinary briefing: Action55 type5 at waypoint1=(80,58).
        // No radar request or gameplay order is injected by this capture.
        if !before.events.iter().any(|event| {
            event.event_type == crate::sim::radar::RadarEventType::EnemyObjectSensed
                && event.cell == (80, 58)
                && event.needs_draw
        }) {
            return Ok(());
        }
        self.saved_radar = Some(before.clone());
        if self.target == OpeningTarget::RadarAlert {
            return Ok(());
        }
        state
            .persistence
            .isolate_capture_saves(&self.save_directory)?;
        let ring_before = state
            .match_state
            .match_presentation
            .minimap
            .as_mut()
            .unwrap()
            .cycle_radar_event(Instant::now());
        ensure!(
            ring_before.is_some(),
            "authored alert did not enter the review ring"
        );
        let path =
            crate::app::persistence::commands::save_game(state, "SOV02 live radar alert", None)?;
        crate::app::persistence::commands::try_load_save_file(state, &path)?;
        let minimap = state
            .match_state
            .match_presentation
            .minimap
            .as_mut()
            .context("incoming saved radar")?;
        let after = minimap.saved_radar_events();
        ensure!(
            before == after,
            "ordinary load discarded saved radar alerts"
        );
        ensure!(
            minimap.cycle_radar_event(Instant::now()).is_none(),
            "load retained outgoing Spacebar history"
        );
        // The ordinary post-load frame refreshes House-derived radar
        // availability. Freezing before it leaves the newly installed radar
        // housing offline, even though its live alert array loaded correctly.
        let receipt =
            crate::app::match_runtime::sim_tick::advance_in_game_runtime_exact_step(state)?;
        let mut expected = crate::render::radar::ClientRadarEvents::from_saved(before.clone())?;
        expected.advance_to_frame(
            receipt.tick_after,
            &state
                .rules()
                .context("restored radar rules")?
                .radar_event_config,
        );
        self.saved_radar = Some(expected.saved());
        self.steps.push(receipt);
        self.restored = Some(
            json!({"path":path,"radar_before":before,"radar_after_load":after,
            "ring_before":ring_before,"ring_after":null,
            "first_resumed_frame":receipt,
            "scope":"ordinary SOV02 briefing Action55 type5 at waypoint1; explicit campaign admission, no preceding mission completion; normal VERA save/load; first radar redraw checked separately"}),
        );
        Ok(())
    }

    #[cfg(feature = "dev-overlays")]
    fn replay_timer_actions(&mut self, state: &mut AppState) -> Result<()> {
        let runtime = state
            .match_state
            .sim_runtime
            .write_sim(SimWrite::Debug)
            .context("timer replay has no runtime")?;
        let sim = &mut runtime.simulation;
        let count = sim
            .dev_replay_scenario_timer_actions(&runtime.resources.rules, "0952F87C")
            .context("SOV01 has no parsed timer actions")?;
        ensure!(
            count == 3 && sim.session().scenario_timer_view() == Some((1800, "MISSION:TmeTimer")),
            "SOV01 timer replay differs from its retail actions"
        );
        self.timer_action_replay = Some(json!({
            "mode": "retail-timer-actions-after-opening-unlock", "trigger": "0952F87C",
            "action_count": count, "tick": sim.session().tick(),
            "binary_frame": sim.session().binary_frame(),
            "scope": "later time-machine/power/Jurassic objectives were not played; only timer actions27/23/103 were replayed",
        }));
        Ok(())
    }

    #[cfg(not(feature = "dev-overlays"))]
    fn replay_timer_actions(&mut self, _state: &mut AppState) -> Result<()> {
        anyhow::bail!("timer action replay requires dev-overlays")
    }

    /// Save before ALL01's first authored lock and again after its unlock,
    /// replay the opening from the first save, then load the unlocked save the
    /// way the Esc menu's Load does (`apply_saved_game_outcome`: open B5, load,
    /// close the menu) while the replayed lock holds.
    fn drive_load_while_locked(&mut self, state: &mut AppState) -> Result<()> {
        use crate::app::persistence::commands::{save_game, try_load_save_file};
        use crate::ui::pause_menu::InGameMenuState;
        if self.load_while_locked.is_some() {
            self.steps_after_load += 1;
            return Ok(());
        }
        let locked = crate::app::input::campaign_input::is_locked(&state.match_state);
        let Some(pre_lock) = self.pre_lock_save.clone() else {
            ensure!(
                !locked,
                "ALL01 locked input before its first presented frame"
            );
            state
                .persistence
                .isolate_capture_saves(&self.save_directory)
                .context("create isolated campaign capture save directory")?;
            let path = save_game(state, "ALL01 before opening lock", None)
                .context("save before the opening lock")?;
            self.pre_lock_save = Some(path);
            return Ok(());
        };
        let Some(unlocked) = self.unlocked_save.clone() else {
            if self.saw_unlock && !locked {
                let path = save_game(state, "ALL01 after opening unlock", None)
                    .context("save after the opening unlock")?;
                self.unlocked_save = Some(path);
                try_load_save_file(state, &pre_lock)
                    .context("replay the opening from the pre-lock save")?;
                self.loaded_frame = Some(installed_frame(state)?);
                self.observe(state)?;
            }
            return Ok(());
        };
        let depth_before = state.match_state.input.cursor_visibility.level();
        if !locked || depth_before >= 0 {
            return Ok(());
        }
        App::enter_in_game_menu_state(state, InGameMenuState::Menu);
        try_load_save_file(state, &unlocked).context("load the unlocked save in the lock")?;
        App::enter_in_game_menu_state(state, InGameMenuState::Closed);
        self.loaded_frame = Some(installed_frame(state)?);
        let locked_after = crate::app::input::campaign_input::is_locked(&state.match_state);
        let depth_after = state.match_state.input.cursor_visibility.level();
        let cursor_instances_after =
            crate::app::presentation::ui_overlays::build_software_cursor_instances(
                state,
                &crate::app::input::cursor::CursorTargets::read(state),
            )
            .0
            .len();
        ensure!(
            !locked_after && depth_after == 0 && cursor_instances_after == 1,
            "the unlocked save loaded in the lock left the pointer hidden \
             (locked {locked_after}, depth {depth_after}, {cursor_instances_after} cursor sprites)"
        );
        self.load_while_locked = Some(json!({
            "depth_before": depth_before, "locked_after": locked_after,
            "depth_after": depth_after, "cursor_instances_after": cursor_instances_after,
            "right_drag_pans_after": probe_right_drag_pans(state),
        }));
        self.observe(state)?;
        Ok(())
    }

    fn observe(&mut self, state: &AppState) -> Result<()> {
        let facts = opening_observation(state)?;
        if self.target == OpeningTarget::RealtimeUnlocked {
            ensure!(
                facts["game_speed"] == 2,
                "ordinary campaign lost its native speed"
            );
            ensure!(
                facts["eva_event"].as_str().is_none_or(|event| !event.eq_ignore_ascii_case("EVA_PsychicDominatorDetected")),
                "unpowered SOV01 Dominator warning interrupted the opening"
            );
        }
        let locked = facts["input_locked"]
            .as_bool()
            .context("lock observation missing")?;
        self.saw_unlock |= self.saw_lock && !locked;
        self.saw_lock |= locked;
        self.saw_countdown |= !facts["scenario_timer"].is_null();
        if let OpeningTarget::TriggerTrace { .. } = self.target {
            return Ok(());
        }
        if !facts["scenario_timer"].is_null() {
            self.last_running_countdown = Some(json!({
                "tick": facts["tick"], "binary_frame": facts["binary_frame"],
                "timer": facts["scenario_timer"],
            }));
        }
        if matches!(
            self.target,
            OpeningTarget::SovietTick(_) | OpeningTarget::SovietNavy(_)
        ) && self.saw_unlock
            && facts["tick"].as_u64().is_some_and(|tick| tick % 120 != 0)
            && self
                .observations
                .last()
                .is_some_and(|last| last["input_locked"] == facts["input_locked"])
        {
            return Ok(());
        }
        // Simulation time is attached to changes; it does not make every
        // moving pixel or every countdown visit into a new observation row.
        let mut projection = facts.clone();
        projection
            .as_object_mut()
            .expect("observation object")
            .remove("tick");
        projection
            .as_object_mut()
            .expect("observation object")
            .remove("binary_frame");
        projection
            .as_object_mut()
            .expect("observation object")
            .remove("wall_ms");
        if let Some(timer) = projection["scenario_timer"].as_object_mut() {
            timer.remove("remaining");
        }
        projection["camera"]
            .as_object_mut()
            .expect("camera object")
            .remove("focus_progress_bits");
        projection["camera"]
            .as_object_mut()
            .expect("camera object")
            .remove("view_center_world_pixels");
        projection["camera"]
            .as_object_mut()
            .expect("camera object")
            .remove("last_ai_frame");
        for building in projection["building_receivers"]
            .as_array_mut()
            .expect("building observations")
        {
            for slot in building["anim_slots"]
                .as_array_mut()
                .expect("animation slot observations")
            {
                slot.as_object_mut().expect("slot object").remove("frame");
            }
        }
        for timer in projection["super_timers"]
            .as_array_mut()
            .expect("super timer observations")
        {
            timer
                .as_object_mut()
                .expect("timer object")
                .remove("remaining");
        }
        if self.previous_projection.as_ref() != Some(&projection) {
            self.previous_projection = Some(projection);
            self.observations.push(facts);
        }
        Ok(())
    }

    /// A bounded play input for the first retail objective. It uses the client
    /// command queue; damage, navigation, reinforcements and triggers remain
    /// entirely in the ordinary simulation.
    fn issue_naval_orders(&mut self, state: &mut AppState) -> Result<()> {
        use crate::sim::command::Command;
        let sim = &mut state
            .match_state
            .sim_runtime
            .write_sim(SimWrite::Debug)
            .context("naval runtime")?
            .simulation;
        let tick = sim.session().tick();
        if tick < 1100 || tick % 30 != 0 {
            return Ok(());
        }
        let viewer = sim.session().current_house().context("campaign player")?;
        let cells = sim
            .resolved_terrain()
            .map(crate::map::resolved_terrain::NativeCellQuery::isolated)
            .context("campaign terrain")?;
        let mut orders = Vec::new();
        for (id, actor) in sim.entities().iter_sorted() {
            if !actor.is_active()
                || actor.lifecycle.in_limbo()
                || sim.interner().resolve(actor.owner()) != "Russians"
                || actor.attack_target.target().is_some()
            {
                continue;
            }
            let kind = sim.interner().resolve(actor.type_ref());
            if !matches!(kind, "SUB" | "SQD" | "DRED") {
                continue;
            }
            let target = sim
                .entities()
                .iter_sorted()
                .filter(|(_, enemy)| enemy.is_active() && !enemy.lifecycle.in_limbo())
                .filter(|(_, enemy)| {
                    !crate::sim::vision::cell_is_shrouded(
                        sim.fog(),
                        &cells,
                        viewer,
                        (enemy.position.rx(), enemy.position.ry()),
                    )
                })
                .filter(|(_, enemy)| {
                    let owner = sim.interner().resolve(enemy.owner());
                    let ty = sim.interner().resolve(enemy.type_ref());
                    if kind == "DRED" {
                        owner == "Alliance" && ty == "GTGCAN"
                    } else {
                        owner == "Americans" && matches!(ty, "CARRIER" | "DEST")
                    }
                })
                .min_by_key(|(_, enemy)| {
                    let dx = i64::from(actor.position.rx()) - i64::from(enemy.position.rx());
                    let dy = i64::from(actor.position.ry()) - i64::from(enemy.position.ry());
                    dx * dx + dy * dy
                })
                .map(|(id, _)| id);
            if let Some(target_id) = target {
                orders.push(Command::Attack {
                    attacker_id: id,
                    target_id,
                });
            }
        }
        for order in orders {
            log::info!("SOV01 capture ordinary player order at {tick}: {order:?}");
            ensure!(
                crate::app::input::commands::schedule_command_in_sim(sim, "Russians", order)
                    .is_some(),
                "naval order rejected"
            );
        }
        Ok(())
    }

    /// Player input only. The stock map owns discovery, creation, recruitment,
    /// movement, damage and cleanup. No Tag or AI action is called here.
    fn issue_campaign_encounter_orders(&mut self, state: &mut AppState) -> Result<()> {
        use crate::sim::combat::TargetKind;
        use crate::sim::command::Command;
        if self.group_orders.is_some() {
            return Ok(());
        }
        let (tick, owner, ids) = armed_player_units(state, false)?;
        if tick < 300 || ids.is_empty() {
            return Ok(());
        }
        let (target, group) = match self.target {
            OpeningTarget::SovietGroups(_) => ((141, 92), Some(0)),
            OpeningTarget::SovietFortresses(_) => ((112, 81), None),
            _ => unreachable!("encounter inputs require a selected stock encounter"),
        };
        if let Some(group) = group {
            ensure!(
                crate::app::input::commands::try_schedule_command(
                    &mut state.match_state,
                    &owner,
                    Command::AssignControlGroup {
                        group,
                        entity_ids: ids.clone()
                    }
                )
                .is_some(),
                "group input rejected"
            );
        }
        for &entity_id in &ids {
            ensure!(
                crate::app::input::commands::try_schedule_command(
                    &mut state.match_state,
                    &owner,
                    Command::AttackMove {
                        entity_id,
                        target: None,
                        destination: Some(TargetKind::Cell(target.0, target.1))
                    }
                )
                .is_some(),
                "attack-move input rejected"
            );
        }
        self.group_orders = Some(json!({"tick": tick, "owner": owner, "ids": ids,
            "requested_native_group": group, "attack_move_cell": target}));
        if group.is_some() {
            camera::center_camera_on_cell(
                &state.platform,
                &state.renderer,
                &state.frontend.screen,
                &mut state.match_state,
                &state.persistence,
                target.0,
                target.1,
            );
        }
        Ok(())
    }

    /// Trigger trace `-push-<X>-<Y>`: like a player sending the whole armed
    /// force, every armed unit attack-moves toward one Cell, again each game
    /// minute so new and idle units follow. Player input only.
    fn issue_trace_push_orders(&mut self, state: &mut AppState) -> Result<()> {
        use crate::sim::combat::TargetKind;
        use crate::sim::command::Command;
        const PERIOD_TICKS: u64 = 900;
        // Engineers carry their own capture orders. The DefuseKit can make
        // them eligible for the armed group, whose attack-move replaces those orders.
        let reserve_engineers = matches!(
            self.target,
            OpeningTarget::TriggerTrace { orders, .. } if orders.capture.is_some()
        );
        let (tick, owner, ids) = armed_player_units(state, reserve_engineers)?;
        let Some(cell) = self.trace_destination(tick) else {
            return Ok(());
        };
        let changed = self.push_destination != Some(cell);
        if ids.is_empty()
            || (!changed
                && self
                    .push_orders
                    .last()
                    .is_some_and(|&(last, _)| tick < last + PERIOD_TICKS))
        {
            return Ok(());
        }
        for &entity_id in &ids {
            ensure!(
                crate::app::input::commands::try_schedule_command(
                    &mut state.match_state,
                    &owner,
                    Command::AttackMove {
                        entity_id,
                        target: None,
                        destination: Some(TargetKind::Cell(cell.0, cell.1)),
                    }
                )
                .is_some(),
                "push attack-move input rejected"
            );
        }
        self.push_destination = Some(cell);
        self.push_orders.push((tick, ids.len()));
        Ok(())
    }

    /// Trigger trace `-film-<TICKS>`: the retail screenshot key, pressed on
    /// the interval the plan names. The pictures are the player's own view of
    /// the battle, taken by the ordinary screenshot path.
    fn request_film_frames(&mut self, state: &mut AppState) -> Result<()> {
        let Some(every) = self.film_every else {
            return Ok(());
        };
        let tick = {
            let runtime = state
                .match_state
                .sim_runtime
                .runtime()
                .context("campaign runtime")?;
            runtime.simulation.session().tick()
        };
        if tick % every != 0 || self.film_ticks.last() == Some(&tick) {
            return Ok(());
        }
        state.match_state.input.retail_screenshot_requested = true;
        self.film_ticks.push(tick);
        Ok(())
    }

    /// The cell the trace's armed force attacks toward. A plan that names
    /// legs moves the force along them: the last leg whose tick has come
    /// names the cell, and no order is sent before the first leg starts.
    /// A plan without legs keeps the push cell.
    fn trace_destination(&self, tick: u64) -> Option<(u16, u16)> {
        Self::destination_for(&self.target, tick)
    }

    /// The cell a trace's armed force attacks toward at a tick.
    fn destination_for(target: &OpeningTarget, tick: u64) -> Option<(u16, u16)> {
        let OpeningTarget::TriggerTrace { orders, .. } = target else {
            return None;
        };
        let Some((legs, count)) = &orders.legs else {
            return orders.push;
        };
        legs.iter()
            .take(*count as usize)
            .filter(|(at, _)| *at <= tick)
            .last()
            .map(|(_, cell)| *cell)
    }

    fn issue_trace_capture(&mut self, state: &mut AppState, cells: &[(u16, u16)]) -> Result<()> {
        use crate::map::entities::EntityCategory;
        use crate::sim::command::Command;
        use crate::sim::world::EngineerBuildingAction;

        let runtime = state
            .match_state
            .sim_runtime
            .runtime()
            .context("campaign runtime")?;
        let sim = &runtime.simulation;
        let rules = &runtime.resources.rules;
        let player = sim.session().current_house().context("campaign player")?;
        // The probe plays the scripted objective with the map's own
        // knowledge: a human player would scout the cell first, the map's
        // objective names the building. The fog reading stays in the receipt.
        let building_at = |(x, y)| {
            let id = sim.occupancy().first_building_on_layer(
                x,
                y,
                crate::sim::movement::locomotor::MovementLayer::Ground,
            )?;
            sim.entities()
                .get(id)
                .filter(|entity| entity.is_ai_alive())
                .map(|entity| (id, entity))
        };
        let tick = sim.session().tick();
        // The capture waits for the armed force to clear the way.
        if self.capture_after.is_some_and(|after| tick < after) {
            return Ok(());
        }
        // Every named cell is reported, so the receipt shows the whole
        // objective and not only the building the probe is after.
        let cell_observations = cells
            .iter()
            .map(|&cell| match building_at(cell) {
                Some((id, entity)) => json!({
                    "id": id,
                    "cell": cell,
                    "type": sim.interner().resolve(entity.type_ref()),
                    "owner": sim.interner().resolve(entity.owner()),
                    "visible": sim.fog().is_cell_visible(player, cell.0, cell.1),
                }),
                None => json!({"cell": cell, "building": Value::Null}),
            })
            .collect::<Vec<_>>();
        let mut probe = json!({"cells": cell_observations});
        // The capture list is worked in the order it was named: the first
        // building the player does not own is the next one.
        // A building an order already claims is not offered again, so a
        // running capture keeps its place in the list.
        let claimed: Vec<u64> = self
            .capture_in_flight
            .iter()
            .filter_map(|entry| self.capture_orders.get(entry.order))
            .map(|order| order.target)
            .collect();
        let target = next_capture_cell(cells, |cell| {
            building_at(cell)
                .map(|(id, entity)| (id, entity.owner() == player || claimed.contains(&id)))
        });
        let in_flight = !self.capture_in_flight.is_empty();
        let engineers = || {
            sim.entities().iter_sorted().filter(|(_, entity)| {
                entity.is_ai_alive()
                    && entity.owner() == player
                    && entity.category() == EntityCategory::Infantry
                    && rules
                        .object(sim.interner().resolve(entity.type_ref()))
                        .is_some_and(|object| object.engineer)
            })
        };
        probe["engineers"] = json!(
            engineers()
                .map(|(id, entity)| {
                    json!({
                        "id": id,
                        "cell": [entity.position.rx(), entity.position.ry()],
                        "mission": format!("{:?}", entity.passive_acquire_mission()),
                    })
                })
                .collect::<Vec<_>>()
        );
        let engineer = if in_flight {
            None
        } else {
            target.and_then(|(target, _)| {
                engineers().find_map(|(id, entity)| {
                    (!entity.lifecycle.in_limbo()
                        && engineer_is_free(
                            entity.mission.current(),
                            entity.mission.queued(),
                            entity.mission.suspended(),
                        )
                        && sim.engineer_building_action(id, target, rules)
                            == Some(EngineerBuildingAction::Capture))
                    .then_some((
                        id,
                        entity.position.rx(),
                        entity.position.ry(),
                        entity.passive_acquire_mission(),
                    ))
                })
            })
        };
        // The engineer the probe watches is the one it ordered, or the one
        // that still carries an order.
        let watched = if in_flight {
            let entry = target
                .and_then(|(id, _)| {
                    self.capture_in_flight
                        .iter()
                        .find(|entry| {
                            self.capture_orders
                                .get(entry.order)
                                .is_some_and(|order| order.target == id)
                        })
                        .copied()
                })
                .or_else(|| self.capture_in_flight.first().copied());
            entry.and_then(|entry| {
                let order = self.capture_orders.get(entry.order)?;
                sim.entities().get(order.engineer).map(|entity| {
                    (
                        order.engineer,
                        entity.position.rx(),
                        entity.position.ry(),
                        entity.passive_acquire_mission(),
                    )
                })
            })
        } else {
            engineer
        };
        let target_owner = target.and_then(|(id, _)| {
            sim.entities()
                .get(id)
                .map(|entity| sim.interner().resolve(entity.owner()).to_owned())
        });
        probe["target"] = match target {
            Some((id, cell)) => json!({"id": id, "cell": [cell.0, cell.1]}),
            None => Value::Null,
        };
        probe["engineer"] = match watched {
            Some((id, x, y, mission)) => json!({
                "id": id, "cell": [x, y], "mission": format!("{mission:?}")}),
            None => Value::Null,
        };
        let has_target = target.is_some();
        let has_engineer = engineer.is_some();
        let mut accepted = false;
        if let (Some((target, cell)), Some((engineer, x, y, _))) = (target, engineer) {
            let owner = sim.interner().resolve(player).to_owned();
            accepted = crate::app::input::commands::try_schedule_command(
                &mut state.match_state,
                &owner,
                Command::CaptureBuilding {
                    engineer_id: engineer,
                    target_building_id: target,
                },
            )
            .is_some();
            if accepted {
                let start_cell = (x, y);
                self.capture_in_flight.push(CaptureInFlight {
                    order: self.capture_orders.len(),
                    carrying: false,
                });
                self.capture_orders.push(CaptureOrder {
                    tick,
                    engineer,
                    target,
                    cell,
                    completed: None,
                    start_cell,
                    last_cell: start_cell,
                    route: vec![(tick, start_cell)],
                    dropped: None,
                    health: Vec::new(),
                    threats: Vec::new(),
                });
            }
        }
        let waiting = if !has_target {
            "no named building is left to capture"
        } else if in_flight {
            "the named building's capture order is still running"
        } else if !has_engineer {
            "no engineer of the player waits for orders"
        } else if !accepted {
            "the capture order was refused"
        } else {
            "capture order issued"
        };
        probe["accepted"] = json!(accepted);
        probe["waiting"] = json!(waiting);
        if !in_flight {
            self.append_capture_log(
                tick,
                json!({
                    "engineer": probe["engineer"],
                    "target": probe["target"],
                    "target_owner": target_owner,
                    "waiting": waiting,
                }),
            );
        }
        self.capture_probe = Some(probe);
        Ok(())
    }

    fn append_capture_log(&mut self, tick: u64, mut entry: Value) {
        if self.capture_log.last().is_some_and(|last| {
            entry.as_object().is_some_and(|fields| {
                fields
                    .iter()
                    .all(|(key, value)| last.get(key) == Some(value))
            })
        }) {
            return;
        }
        entry["tick"] = json!(tick);
        self.capture_log.push(entry);
    }

    /// Update only the active attempts, including on the terminal frame.
    fn note_capture_progress(&mut self, state: &AppState) {
        if self.capture_in_flight.is_empty() {
            return;
        }
        let Some(runtime) = state.match_state.sim_runtime.runtime() else {
            return;
        };
        let sim = &runtime.simulation;
        let Some(player) = sim.session().current_house() else {
            return;
        };
        let tick = sim.session().tick();
        let attempts = self.capture_in_flight.clone();
        let mut running = Vec::new();
        let mut entries = Vec::new();
        for mut attempt in attempts {
            let Some(order) = self.capture_orders.get_mut(attempt.order) else {
                continue;
            };
            let target = sim.entities().get(order.target);
            let engineer = sim.entities().get(order.engineer);
            let mission = engineer.map(|entity| entity.passive_acquire_mission());
            // The walk's own state: the cell the engineer walks to, the steps
            // left in its route and the coordinate the building answers as a
            // navigation goal. They say whether the order kept a goal and
            // whether the route ran out before it.
            let walk_goal = engineer.and_then(|entity| {
                entity
                    .locomotor
                    .as_ref()
                    .and_then(|locomotor| locomotor.walk_destination())
                    .map(|coord| ((coord.x / 256) as u16, (coord.y / 256) as u16))
            });
            let steps_left =
                engineer.map(|entity| entity.navigation.path_replay().remaining_directions().len());
            let nav_goal = sim
                .navigation_target_coordinate(
                    crate::sim::components::NavTargetRef::object(order.target),
                    Some(order.engineer),
                    &runtime.resources.rules,
                )
                .ok()
                .map(|coord| ((coord.x / 256) as i16 as u16, (coord.y / 256) as i16 as u16));
            if let Some(entity) = engineer {
                order.last_cell = (entity.position.rx(), entity.position.ry());
                // The walk is the evidence for how the order ended: the route
                // shows the cells the engineer passed and the threats the
                // receipt names are the ones next to it when it was hit.
                if order
                    .route
                    .last()
                    .is_none_or(|&(_, cell)| cell != order.last_cell)
                {
                    order.route.push((tick, order.last_cell));
                    if order.route.len() > 64 {
                        order.route.remove(0);
                    }
                }
                // The engineer's health and the units next to it are the
                // evidence for how the order ended: infantry leave the world
                // at death, so the last reading is what killed them.
                let health = entity.health.current();
                if order.health.last().map(|&(_, value)| value) != Some(health) {
                    order.health.push((tick, health));
                    if order.health.len() > 12 {
                        order.health.remove(0);
                    }
                    order.threats = threats_near(sim, player, order.engineer, order.last_cell);
                }
            }
            let waiting = if target.is_some_and(|entity| entity.owner() == player) {
                order.completed = Some(tick);
                "the building changed hands"
            } else {
                // The engineer carries the order while it lives. The walk to the
                // building reports the mission the walk runs, not the capture
                // order, so only the engineer's own end ends the attempt.
                let dropped = match engineer {
                    _ if target.is_none_or(|entity| !entity.is_ai_alive()) => {
                        Some("the building is gone")
                    }
                    None => Some("the engineer is gone"),
                    Some(entity) if !entity.is_ai_alive() => Some("the engineer died"),
                    Some(entity) if entity.owner() != player => Some("the engineer changed sides"),
                    Some(entity) if entity.lifecycle.in_limbo() => {
                        Some("the engineer left the field")
                    }
                    Some(_) if mission == Some(MissionType::Capture) => {
                        attempt.carrying = true;
                        None
                    }
                    Some(_)
                        if mission != Some(MissionType::Capture)
                            && tick.saturating_sub(order.tick) > CAPTURE_ORDER_GRACE_TICKS =>
                    {
                        Some("the engineer is no longer walking to the building")
                    }
                    Some(_) => None,
                };
                order.dropped = dropped;
                dropped.unwrap_or("the named building's capture order is still running")
            };
            let entry = json!({
                "engineer": {
                    "id": order.engineer,
                    "cell": order.last_cell,
                    "mission": mission.map(|mission| format!("{mission:?}")),
                    "health": order.health.last().map(|&(_, value)| value),
                    "walk_goal": walk_goal.map(|(x, y)| json!([x, y])),
                    "steps_left": steps_left,
                },
                "target": {"id": order.target, "cell": order.cell},
                "nav_goal": nav_goal.map(|(x, y)| json!([x, y])),
                "target_owner": target.map(|entity| sim.interner().resolve(entity.owner())),
                "capture_mission": attempt.carrying,
                "waiting": waiting,
            });
            let live = order.completed.is_none() && order.dropped.is_none();
            if live {
                running.push(attempt);
            }
            entries.push(entry);
        }
        self.capture_in_flight = running;
        for entry in entries {
            self.append_capture_log(tick, entry);
        }
    }

    pub(super) fn receipt(&self, state: &AppState) -> Result<Value> {
        ensure!(
            self.ready(state)?,
            "opening readback missed its selected checkpoint"
        );
        let mut receipt = json!({
            "target": self.target.phase(), "initial_runtime": self.initial_runtime,
            "exact_steps": self.steps, "observations": self.observations,
            "saw_lock": self.saw_lock, "saw_unlock": self.saw_unlock,
            "saw_countdown": self.saw_countdown,
            "last_running_countdown": self.last_running_countdown,
            "timer_action_replay": self.timer_action_replay,
            "locked_arrow_probe": self.lock_probe, "unlocked_arrow_probe": self.unlock_probe,
            "locked_mouse_probe": self.locked_mouse_probe,
            "unlocked_mouse_probe": self.unlocked_mouse_probe,
            "save_restore": self.restored, "current": opening_observation(state)?,
            "retained_radar": self.saved_radar,
            "radar_display": state.match_state.match_presentation.radar_anim.as_ref()
                .map(|radar| json!({"phase": format!("{:?}", radar.phase()),
                    "minimap_visible": radar.is_minimap_visible()})),
            "pre_lock_save": self.pre_lock_save, "unlocked_save": self.unlocked_save,
            "load_while_locked": self.load_while_locked,
            "group_orders": self.group_orders,
            "requested_tick": match self.target { OpeningTarget::AlliedTick(tick) | OpeningTarget::SovietTick(tick) | OpeningTarget::SovietNavy(tick) | OpeningTarget::SovietGroups(tick) | OpeningTarget::SovietFortresses(tick) | OpeningTarget::TriggerTrace { tick, .. } => Some(tick), _ => None },
            "trigger_springs": self.trigger_springs,
            "trigger_events": self.trigger_events,
            "push_orders": self.push_orders,
            "outcome": trace_outcome(state),
            "outcome_frame": self.outcome_frame,
            "houses": if matches!(self.target, OpeningTarget::TriggerTrace { .. }) {
                Some(trace_houses(state)?)
            } else { None },
            "encounter_entities": if matches!(self.target, OpeningTarget::SovietTick(_) | OpeningTarget::SovietNavy(_) | OpeningTarget::SovietGroups(_) | OpeningTarget::SovietFortresses(_)) {
                let sim = &state.match_state.sim_runtime.runtime().context("encounter runtime")?.simulation;
                Some(sim.entities().iter_sorted().map(|(_, entity)| json!({
                    "id": entity.stable_id(), "type": sim.interner().resolve(entity.type_ref()),
                    "owner": sim.interner().resolve(entity.owner()), "active": entity.is_active(),
                    "limbo": entity.lifecycle.in_limbo(),
                    "cell": [entity.position.rx(), entity.position.ry()],
                    "health": entity.health.current(), "mission": format!("{:?}", entity.mission),
                    "group": entity.object_group(),
                    "target": format!("{:?}", entity.attack_target),
                    "spawn_manager": entity.spawn_manager,
                })).collect::<Vec<_>>())
            } else { None },
            "aircraft_raster": if matches!(self.target, OpeningTarget::AlliedTick(_)) {
                Some(aircraft::observe(state, &self.save_directory)?)
            } else { None },
            "clock_domain": if self.target == OpeningTarget::RealtimeUnlocked {
                "ordinary wall-clock frame pacing and audio service"
            } else {
                "one exact production sim/app frame per presented game frame; audio/message service uses its ordinary wall clock"
            },
        });
        if !self.capture_orders.is_empty() {
            let sim = &state
                .match_state
                .sim_runtime
                .runtime()
                .context("campaign runtime")?
                .simulation;
            let mut captures = Vec::new();
            for order in &self.capture_orders {
                let building = sim.entities().get(order.target);
                captures.push(json!({
                    "tick": order.tick,
                    "engineer_id": order.engineer,
                    "target_id": order.target,
                    "cell": [order.cell.0, order.cell.1],
                    "completed_tick": order.completed,
                    "engineer_start_cell": [order.start_cell.0, order.start_cell.1],
                    "engineer_last_cell": [order.last_cell.0, order.last_cell.1],
                    "route": order.route.iter().map(|&(tick, cell)| json!({"tick": tick, "cell": [cell.0, cell.1]})).collect::<Vec<_>>(),
                    "dropped": order.dropped,
                    "health": order.health.iter().map(|&(tick, value)| json!({"tick": tick, "value": value})).collect::<Vec<_>>(),
                    "threats": order.threats,
                    "final_owner": building.map(|entity| sim.interner().resolve(entity.owner())),
                    "type": building.map(|entity| sim.interner().resolve(entity.type_ref())),
                }));
            }
            receipt["captures"] = json!(captures);
            if let Some(last) = captures.last() {
                receipt["capture"] = last.clone();
            }
        }
        if let Some(probe) = &self.capture_probe {
            receipt["capture_probe"] = probe.clone();
        }
        if let OpeningTarget::TriggerTrace {
            orders:
                TraceOrders {
                    legs: Some((legs, count)),
                    ..
                },
            ..
        } = &self.target
        {
            receipt["push_legs"] = json!(
                legs.iter()
                    .take(*count as usize)
                    .map(|(at, cell)| json!({"tick": at, "cell": cell}))
                    .collect::<Vec<_>>()
            );
        }
        if !self.capture_log.is_empty() {
            receipt["capture_log"] = json!(self.capture_log);
        }
        if !self.film_ticks.is_empty() {
            receipt["film_ticks"] = json!(self.film_ticks);
        }
        if matches!(
            self.target,
            OpeningTarget::TriggerTrace {
                orders: TraceOrders {
                    capture: Some(_),
                    ..
                },
                ..
            }
        ) {
            let sim = &state
                .match_state
                .sim_runtime
                .runtime()
                .context("campaign runtime")?
                .simulation;
            if let Some(player) = sim.session().current_house()
                && let Some(power) = sim.power_states().get(&player)
            {
                receipt["power"] = json!({
                    "house": sim.interner().resolve(player),
                    "output": power.total_output(),
                    "drain": power.total_drain(),
                    "low_power": power.is_low_power(),
                });
            }
        }
        Ok(receipt)
    }
}

fn installed_frame(state: &AppState) -> Result<(u64, u32)> {
    let session = &state
        .match_state
        .sim_runtime
        .runtime()
        .context("no installed runtime")?
        .simulation
        .session();
    Ok((session.tick(), session.binary_frame()))
}

fn mid_pan(view: &crate::sim::snapshot::SavedTacticalView) -> bool {
    let progress = f32::from_bits(view.focus_progress_bits);
    view.focus_rate_bits != 0 && (0.25..1.0).contains(&progress)
}

fn opening_observation(state: &AppState) -> Result<Value> {
    let sim = &state
        .match_state
        .sim_runtime
        .runtime()
        .context("opening observation has no runtime")?
        .simulation;
    let viewer = crate::app::input::commands::preferred_local_owner_name(&state.match_state)
        .and_then(|name| sim.interner().get(&name));
    // Read the same CellClass shroud owner as world drawing. Diagnostic misses
    // retain a separate Dummy, so observing a capture never changes the sim.
    let cells = sim
        .resolved_terrain()
        .map(crate::map::resolved_terrain::NativeCellQuery::isolated);
    let cell_shrouded = |cell| {
        viewer.zip(cells.as_ref()).map(|(viewer, cells)| {
            crate::sim::vision::cell_is_shrouded(sim.fog(), cells, viewer, cell)
        })
    };
    let mut active_types = std::collections::BTreeMap::<String, usize>::new();
    let mut flashing = Vec::new();
    let mut building_receivers = Vec::new();
    let mut aircraft = Vec::new();
    for (_, entity) in sim
        .entities()
        .iter_sorted()
        .filter(|(_, entity)| entity.is_active())
    {
        let name = sim.interner().resolve(entity.type_ref()).to_owned();
        *active_types.entry(name.clone()).or_default() += 1;
        if entity.category() == crate::map::entities::EntityCategory::Aircraft {
            aircraft.push(json!({
                "id": entity.stable_id(), "type": name,
                "cell": [entity.position.rx(), entity.position.ry()],
                "world_z": entity.position.exact_z_leptons(),
                "anchor": crate::render::locomotor_visual::screen_position(entity),
                "voxel": entity.is_voxel(),
                "display_layer": sim.display_layers().layer_of(entity.stable_id()).map(|layer| layer.index()),
                "crashing": entity.crash.is_crashing(),
                "health": entity.health.current(),
            }));
        }
        if entity.category() == crate::map::entities::EntityCategory::Structure {
            // Observe existing damage/power owners after exact production
            // frames; this must never substitute a receiver or recalculate power.
            building_receivers.push(json!({
                "id": entity.stable_id(), "type": name,
                "owner": sim.interner().resolve(entity.owner()),
                "cell": [entity.position.rx(), entity.position.ry()],
                "health": entity.health.current(),
                "estimated_health": entity.estimated_health.get(),
                "sampled_power_health": entity.building_power_health_sample(),
                "damaged_mode": entity.building_art.damage_state_active(),
                "damage_smoke_system": entity.seldom.damage_smoke_system_id,
                "anim_slots": entity.building_art.anim_slots().iter().enumerate()
                    .filter_map(|(slot, id)| {
                        let id = (*id)?;
                        let anim = sim.anim(id)?;
                        Some(json!({"slot": slot, "id": id,
                            "type": sim.interner().resolve(anim.type_id()),
                            "frame": anim.runtime().current_frame,
                            "paused": anim.runtime().paused}))
                    }).collect::<Vec<_>>(),
                "house_power": sim.power_states().get(&entity.owner()).map(|power| json!({
                    "output": power.total_output(), "drain": power.total_drain(),
                    "low_power": power.is_low_power(),
                })),
            }));
        }
        if entity.flashing.flashed() {
            let cell = (entity.position.rx(), entity.position.ry());
            flashing.push(json!({"id": entity.stable_id(), "type": name,
                "cell": cell, "cell_shrouded": cell_shrouded(cell)}));
        }
    }
    let markers = sim
        .anims()
        .filter(|(_, anim)| anim.no_effects())
        .map(|(id, anim)| {
            let cell = (
                crate::util::lepton::lepton_to_cell_packed(anim.world_coord().x) as u16,
                crate::util::lepton::lepton_to_cell_packed(anim.world_coord().y) as u16,
            );
            json!({"id": id, "type": sim.interner().resolve(anim.type_id()),
            "coord": [anim.world_coord().x, anim.world_coord().y, anim.world_coord().z],
            "z_adjust": anim.z_adjust(), "cell": cell,
            "cell_shrouded": cell_shrouded(cell)})
        })
        .collect::<Vec<_>>();
    let teams = sim
        .team_script_vm()
        .teams_in_order()
        .map(|team| {
            json!({
                "id": team.id(), "type": team.team_type_id().map(|id| sim.interner().resolve(id)),
                "script": sim.interner().resolve(team.script_id()), "cursor": team.cursor(),
                "members": team.members().collect::<Vec<_>>(),
                "transports": team.members().filter_map(|id| {
                    let entity = sim.entities().get(id)?;
                    let cargo = entity.passenger_role().cargo()?;
                    Some(json!({"id": id, "cell": [entity.position.rx(), entity.position.ry()],
                        "passengers": cargo.passengers()}))
                }).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let messages = state.match_state.match_presentation.message_list.messages().iter()
        .map(|message| json!({"text": message.text, "rgb": message.rgb, "deadline_ms": message.deadline_ms}))
        .collect::<Vec<_>>();
    Ok(json!({
        "tick": sim.session().tick(), "binary_frame": sim.session().binary_frame(),
        "input_locked": sim.session().input_locked(),
        "scenario_timer": sim.session().scenario_timer_view().map(|(remaining, caption)|
            json!({"remaining": remaining, "caption": caption})),
        "cursor_depth": state.match_state.input.cursor_visibility.level(),
        "camera": camera::saved_tactical_view(&state.platform, &state.renderer, &state.frontend.screen, &state.match_state, &state.persistence),
        "camera_origin": [state.match_state.input.camera_x, state.match_state.input.camera_y],
        "fog_viewer": viewer.map(|viewer| sim.interner().resolve(viewer)),
        "active_types": active_types, "flashing": flashing, "no_effects_anims": markers,
        "building_receivers": building_receivers, "aircraft": aircraft,
        "super_timers": state.rules().map(|rules|
            crate::sim::superweapon::super_timer_views(sim, rules).into_iter().map(|view|
                json!({"owner": sim.interner().resolve(view.owner()),
                    "type": sim.interner().resolve(view.sw_type()),
                    "remaining": view.frames_left(), "held": view.on_hold(),
                    "recharge": view.recharge_frames(), "place": view.place()}))
                .collect::<Vec<_>>()).unwrap_or_default(),
        "teams": teams, "messages": messages,
        "eva_active": state.audio.sfx_player.as_ref().is_some_and(|sfx| sfx.voices_active()),
        "eva_event": state.audio.sfx_player.as_ref().and_then(|sfx| sfx.current_eva_event()),
        "game_speed": sim.session().game_options().game_speed,
        "wall_ms": crate::app::match_runtime::frame_pacer::wall_clock_ms(),
    }))
}

/// Use the same literal-key entry point as a window event. A changed user
/// binding is reported rather than invoking its unrelated gameplay command.
fn probe_arrow_edge(state: &mut AppState, locked: bool) -> Result<Value> {
    let key = Key::Named(NamedKey::ArrowRight);
    let resolution = state.match_state.input.hotkey_bindings.resolve_event(
        &key,
        KeyLocation::Standard,
        state.match_state.input.hotkey_modifiers,
    );
    let Some(scroll_key) = hotkeys::fallback_scroll_key(resolution) else {
        return Ok(json!({"locked": locked, "custom_binding": true}));
    };
    for edge_state in [ElementState::Pressed, ElementState::Released] {
        keyboard::in_game_key_edge(
            state,
            keyboard::InGameKeyEdge {
                physical: KeyCode::ArrowRight,
                logical: &key,
                unmodified: &key,
                location: KeyLocation::Standard,
                state: edge_state,
                repeat: false,
                text: None,
            },
            false,
        );
        if edge_state.is_pressed() {
            ensure!(
                state.match_state.input.keys_held.contains(&scroll_key) == !locked,
                "ordinary arrow press admission differs from the Scenario lock"
            );
        }
    }
    ensure!(
        !state.match_state.input.keys_held.contains(&scroll_key),
        "ordinary arrow release retained its scroll latch"
    );
    Ok(json!({"locked": locked, "press_admitted": !locked,
        "release_cleared": true, "custom_binding": false}))
}

/// Ordinary pointer edges remain physical input even when6930A0 refuses the
/// tactical message. The native per-frame consumer692F30 reads live buttons
/// through54F5C0->53EC90; fast_scroll.json update_cases pins its priority.
fn probe_locked_mouse_edges(state: &mut AppState) -> Result<Value> {
    use crate::app::input::dispatch::handle_mouse_input;
    ensure!(
        state.match_state.input.tactical_mouse.left_held(),
        "authored lock discarded the pre-lock physical left press"
    );
    ensure!(
        !state.match_state.input.tactical_mouse.captured
            && !state.match_state.input.selection_state.is_band_box_active(),
        "authored lock retained the pre-lock tactical gesture"
    );
    handle_mouse_input(state, MouseButton::Left, ElementState::Released);
    ensure!(
        !state.match_state.input.tactical_mouse.left_held(),
        "physical left release during the input lock was discarded"
    );
    handle_mouse_input(state, MouseButton::Left, ElementState::Pressed);
    ensure!(
        state.match_state.input.tactical_mouse.left_held()
            && !state.match_state.input.tactical_mouse.captured,
        "locked left press must update physical state without taking capture"
    );
    handle_mouse_input(state, MouseButton::Right, ElementState::Pressed);
    ensure!(
        state.match_state.input.tactical_mouse.right_held()
            && !state.match_state.input.tactical_mouse.captured,
        "locked right press must update physical state without taking capture"
    );
    handle_mouse_input(state, MouseButton::Right, ElementState::Released);
    ensure!(
        !state.match_state.input.tactical_mouse.right_held(),
        "physical right release during the input lock was discarded"
    );
    Ok(
        json!({"pre_lock_capture_cancelled": true, "locked_release_cleared": true,
        "locked_press_recorded": true, "left_held_until_unlock": true,
        "locked_press_captured": false}),
    )
}

/// Whether an ordinary right drag over the battlefield requests a pan, that
/// is, whether the mouse reaches the tactical view.
fn probe_right_drag_pans(state: &mut AppState) -> bool {
    use crate::app::input::dispatch::{handle_cursor_moved_in_game, handle_mouse_input};
    let cursor = (
        state.match_state.input.cursor_x,
        state.match_state.input.cursor_y,
    );
    state.match_state.input.cursor_x = 200.0;
    state.match_state.input.cursor_y = 200.0;
    handle_cursor_moved_in_game(state);
    handle_mouse_input(state, MouseButton::Right, ElementState::Pressed);
    state.match_state.input.cursor_x = 280.0;
    handle_cursor_moved_in_game(state);
    let pan = camera::poll_mouse_scroll(
        &state.platform,
        &state.renderer,
        &state.frontend,
        &mut state.match_state,
        &state.persistence,
    );
    handle_mouse_input(state, MouseButton::Right, ElementState::Released);
    (
        state.match_state.input.cursor_x,
        state.match_state.input.cursor_y,
    ) = cursor;
    handle_cursor_moved_in_game(state);
    pan != (0.0, 0.0)
}

fn probe_unlocked_mouse_edges(state: &mut AppState) -> Result<Value> {
    use crate::app::input::dispatch::{handle_cursor_moved_in_game, handle_mouse_input};
    ensure!(
        state.match_state.input.tactical_mouse.left_held(),
        "physical left press did not survive until authored unlock"
    );
    let cursor = (
        state.match_state.input.cursor_x,
        state.match_state.input.cursor_y,
    );
    state.match_state.input.cursor_x = 200.0;
    state.match_state.input.cursor_y = 200.0;
    handle_cursor_moved_in_game(state);
    handle_mouse_input(state, MouseButton::Right, ElementState::Pressed);
    state.match_state.input.cursor_x = 240.0;
    handle_cursor_moved_in_game(state);
    ensure!(
        camera::poll_mouse_scroll(
            &state.platform,
            &state.renderer,
            &state.frontend,
            &mut state.match_state,
            &state.persistence
        ) == (0.0, 0.0),
        "right drag panned despite the left button held across unlock"
    );
    handle_mouse_input(state, MouseButton::Right, ElementState::Released);
    handle_mouse_input(state, MouseButton::Left, ElementState::Released);
    handle_mouse_input(state, MouseButton::Right, ElementState::Pressed);
    state.match_state.input.cursor_x = 280.0;
    handle_cursor_moved_in_game(state);
    let pan = camera::poll_mouse_scroll(
        &state.platform,
        &state.renderer,
        &state.frontend,
        &mut state.match_state,
        &state.persistence,
    );
    ensure!(
        pan.0 != 0.0 || pan.1 != 0.0,
        "ordinary right drag remained blocked after physical releases"
    );
    handle_mouse_input(state, MouseButton::Right, ElementState::Released);
    ensure!(
        !state.match_state.input.tactical_mouse.captured
            && !state.match_state.input.tactical_mouse.left_held()
            && !state.match_state.input.tactical_mouse.right_held(),
        "pointer regression probe retained a physical button or capture"
    );
    (
        state.match_state.input.cursor_x,
        state.match_state.input.cursor_y,
    ) = cursor;
    handle_cursor_moved_in_game(state);
    Ok(json!({"held_left_blocks_right_pan": true,
        "released_buttons_admit_right_pan": true, "capture_released": true,
        "camera_request_committed": false}))
}

/// The current tick, the player's House name and the armed, active, on-map
/// units a player would send: no buildings, harvesters or unarmed units.
fn armed_player_units(
    state: &AppState,
    reserve_engineers: bool,
) -> Result<(u64, String, Vec<u64>)> {
    let sim = &state
        .match_state
        .sim_runtime
        .runtime()
        .context("campaign runtime")?
        .simulation;
    let viewer = sim.session().current_house().context("campaign player")?;
    let rules = state.rules().context("campaign rules")?;
    let ids = sim
        .entities()
        .iter_sorted()
        .filter_map(|(id, actor)| {
            let object = rules.object(sim.interner().resolve(actor.type_ref()))?;
            (actor.owner() == viewer
                && actor.is_active()
                && !actor.lifecycle.in_limbo()
                && actor.category() != crate::map::entities::EntityCategory::Structure
                && !object.resource_gatherer
                && !(reserve_engineers && object.engineer)
                && crate::sim::combat::combat_weapon::is_armed(
                    actor,
                    object,
                    sim.entities(),
                    rules,
                    sim.interner(),
                ))
            .then_some(id)
        })
        .collect();
    Ok((
        sim.session().tick(),
        sim.interner().resolve(viewer).to_owned(),
        ids,
    ))
}

/// A trigger trace stops on the frame that admits the mission's result.
///
/// Native does not commit that frame: Main_Tick (0x0055DE4F..0x0055DEC8)
/// skips commit and deferred destruction while the outcome waits, so the
/// exact-step receipt cannot show the one tick it demands. Every other
/// capture still fails loudly on that frame, and so does every other
/// failure of the step.
fn trace_stops_on_outcome_frame(
    target: &OpeningTarget,
    error: &ExactStepError,
    sim: &Simulation,
    owner: Option<InternedId>,
) -> bool {
    if !matches!(target, OpeningTarget::TriggerTrace { .. }) {
        return false;
    }
    let Some(owner) = owner else {
        return false;
    };
    match error {
        // The outcome frame advances no tick, including while the House
        // waits for Vox before publishing its request to the session.
        ExactStepError::TickDelta { actual: 0 } => trace_outcome_for(sim, owner).is_some(),
        // The app left the in-game route, which it does once it routed the
        // result.
        ExactStepError::ScreenNotInGame => routed_outcome_for(sim, owner).is_some(),
        _ => false,
    }
}

/// The local player's campaign result once HouseClass::Update admits it.
fn trace_outcome(state: &AppState) -> Option<&'static str> {
    let sim = &state.match_state.sim_runtime.runtime()?.simulation;
    trace_outcome_for(sim, sim.session().current_house()?)
}

/// The result the session routed for one owner.
///
/// This is the query the app routes the result with
/// (Main_Game 0x0048CE8A..0x0048CEA1): the session owns the request after
/// the House's Vox wait.
fn routed_outcome_for(sim: &Simulation, owner: InternedId) -> Option<&'static str> {
    let outcome = sim.ready_outcome_for_owner(owner)?;
    Some(outcome_name(outcome.kind))
}

fn outcome_name(kind: crate::sim::house_state::HouseOutcomeKind) -> &'static str {
    use crate::sim::house_state::HouseOutcomeKind;
    match kind {
        HouseOutcomeKind::Victory => "won",
        HouseOutcomeKind::Defeat => "lost",
    }
}

/// HouseClass::Update 0x004F85BE/0x004F86FE enters the Vox wait only
/// after the result timer and AllowWin checks admit the request.
fn trace_outcome_for(sim: &Simulation, owner: InternedId) -> Option<&'static str> {
    if sim.session().current_house() != Some(owner) {
        return None;
    }
    routed_outcome_for(sim, owner).or_else(|| sim.pending_outcome_wait().map(outcome_name))
}

/// End-of-trace House summary: live, non-limbo objects per category.
fn trace_houses(state: &AppState) -> Result<Value> {
    let sim = &state
        .match_state
        .sim_runtime
        .runtime()
        .context("trigger trace runtime")?
        .simulation;
    let mut houses = std::collections::BTreeMap::<String, [u32; 4]>::new();
    for (_, entity) in sim.entities().iter_sorted() {
        if !entity.is_active() || entity.lifecycle.in_limbo() {
            continue;
        }
        use crate::map::entities::EntityCategory;
        let slot = match entity.category() {
            EntityCategory::Structure => 0,
            EntityCategory::Unit => 1,
            EntityCategory::Infantry => 2,
            EntityCategory::Aircraft => 3,
        };
        houses
            .entry(sim.interner().resolve(entity.owner()).to_owned())
            .or_default()[slot] += 1;
    }
    Ok(json!(
        houses
            .into_iter()
            .map(|(house, [buildings, units, infantry, aircraft])| json!({
                "house": house, "buildings": buildings, "units": units,
                "infantry": infantry, "aircraft": aircraft,
            }))
            .collect::<Vec<_>>()
    ))
}

#[cfg(all(test, tests_app))]
mod tests {
    use super::*;
    use crate::sim::house_state::{HouseOutcomeKind, HouseState};
    use crate::sim::runtime::SimRuntime;
    use crate::sim::scenario_session::ScenarioGameMode;
    use crate::sim::world::{FrameEffects, OutcomeWaitBoundary, TickLane};
    use std::cell::Cell;

    /// The Vox cue the outcome waits for before the House releases it.
    struct Vox(Cell<bool>);

    impl OutcomeWaitBoundary for Vox {
        fn wait_complete(&mut self, _owner: InternedId, _kind: HouseOutcomeKind) -> bool {
            self.0.get()
        }
    }

    fn trace() -> OpeningTarget {
        OpeningTarget::TriggerTrace {
            scenario: "ALL01UMD",
            tick: 900,
            orders: TraceOrders::default(),
        }
    }

    /// A mission whose result waits for its Vox cue.
    fn waiting_mission() -> (SimRuntime, InternedId, Vox) {
        let mut sim = Simulation::with_seed(0x004F_860B);
        let owner = sim.intern("Human");
        let mut house = HouseState::new(owner, 0, None, true, 0, 10);
        assert!(house.accept_win_result(0, 0, false));
        sim.houses_mut_for_test().register(house);
        sim.session_mut_for_test()
            .set_game_mode(ScenarioGameMode::Skirmish);
        sim.session_mut_for_test()
            .set_current_house_for_test(Some(owner));
        let runtime = SimRuntime::from_simulation(sim);
        (runtime, owner, Vox(Cell::new(false)))
    }

    #[test]
    fn the_capture_probe_works_its_list_in_order() {
        // The first named building the player does not own is the next one to
        // capture; a cell without a building is skipped, and a building the
        // player already owns is done.
        let next = next_capture_cell(&[(10, 10), (15, 15), (20, 20)], |cell| match cell {
            (10, 10) => Some((1, true)),
            (20, 20) => Some((2, false)),
            _ => None,
        });
        assert_eq!(next, Some((2, (20, 20))));
        // Nothing is left to capture.
        assert_eq!(next_capture_cell(&[(10, 10)], |_| Some((1, true))), None);
    }

    #[test]
    fn only_an_engineer_without_an_order_takes_the_capture() {
        let none = MissionId::NONE;
        let guard = MissionId::from_known(MissionType::Guard);
        let capture = MissionId::from_known(MissionType::Capture);
        let moving = MissionId::from_known(MissionType::Move);
        assert!(engineer_is_free(none, none, none));
        assert!(engineer_is_free(guard, none, none));
        assert!(!engineer_is_free(capture, none, none));
        assert!(!engineer_is_free(guard, moving, none));
        assert!(!engineer_is_free(guard, none, moving));
    }

    #[test]
    fn the_legs_name_the_cell_the_force_attacks_toward() {
        let mut legs: TraceLegs = [(0, (0, 0)); MAX_TRACE_LEGS];
        legs[0] = (300, (90, 107));
        legs[1] = (1500, (48, 89));
        let target = OpeningTarget::TriggerTrace {
            scenario: "ALL03UMD",
            tick: 1800,
            orders: TraceOrders {
                legs: Some((legs, 2)),
                ..TraceOrders::default()
            },
        };
        // Before the first leg no order goes out.
        assert_eq!(OpeningCapture::destination_for(&target, 299), None);
        // Each leg holds until the next one starts.
        assert_eq!(
            OpeningCapture::destination_for(&target, 300),
            Some((90, 107))
        );
        assert_eq!(
            OpeningCapture::destination_for(&target, 1499),
            Some((90, 107))
        );
        assert_eq!(
            OpeningCapture::destination_for(&target, 1500),
            Some((48, 89))
        );
        // A plan without legs keeps its push cell.
        let push = OpeningTarget::TriggerTrace {
            scenario: "ALL01UMD",
            tick: 900,
            orders: TraceOrders {
                push: Some((112, 120)),
                ..TraceOrders::default()
            },
        };
        assert_eq!(OpeningCapture::destination_for(&push, 1), Some((112, 120)));
    }

    #[test]
    fn the_threats_are_the_hostile_units_next_to_the_engineer() {
        // A hostile unit within reach is listed, one that cannot have reached
        // the engineer is not, and the engineer never appears.
        let candidates = vec![
            (1, "ENGINEER".to_owned(), (10, 10), false),
            (2, "YAGGUN".to_owned(), (12, 11), true),
            (3, "E1".to_owned(), (20, 20), false),
        ];
        let threats = select_threats(1, (10, 10), &candidates);
        assert_eq!(
            threats,
            vec!["YAGGUN#2 at (12,11) targets the engineer".to_owned()]
        );
        // A hostile unit four cells away is out of reach.
        let candidates = vec![(2, "YAGGUN".to_owned(), (14, 10), false)];
        assert!(select_threats(1, (10, 10), &candidates).is_empty());
    }

    #[test]
    fn a_trigger_trace_stops_on_the_outcome_frame_and_other_captures_do_not() {
        let (mut runtime, owner, mut vox) = waiting_mission();
        assert_eq!(trace_outcome_for(&runtime.simulation, owner), None);
        assert!(!trace_stops_on_outcome_frame(
            &trace(),
            &ExactStepError::TickDelta { actual: 0 },
            &runtime.simulation,
            Some(owner),
        ));
        let first = runtime
            .advance_frame(
                &[],
                67,
                TickLane::Ordinary,
                &mut FrameEffects::empty().with_outcome_wait(&mut vox),
            )
            .unwrap();
        assert!(first.outcome_waiting);
        let sim = &runtime.simulation;
        assert_eq!(trace_outcome_for(sim, owner), Some("won"));

        // The frame that admits the result advances no tick: the trace stops
        // there instead of failing the step.
        assert!(trace_stops_on_outcome_frame(
            &trace(),
            &ExactStepError::TickDelta { actual: 0 },
            sim,
            Some(owner),
        ));
        // A step that failed for another reason is still an error.
        assert!(!trace_stops_on_outcome_frame(
            &trace(),
            &ExactStepError::TickDelta { actual: 2 },
            sim,
            Some(owner),
        ));
        assert!(!trace_stops_on_outcome_frame(
            &trace(),
            &ExactStepError::FrameDelta { actual: 0 },
            sim,
            Some(owner),
        ));
        // Every other capture still fails on the frame native does not commit.
        assert!(!trace_stops_on_outcome_frame(
            &OpeningTarget::AlliedTick(900),
            &ExactStepError::TickDelta { actual: 0 },
            sim,
            Some(owner),
        ));
    }

    #[test]
    fn a_routed_result_outranks_the_cleared_house_flag() {
        let (mut runtime, owner, mut vox) = waiting_mission();

        // A mission result ends the mission: the session routes it, the
        // House's own flag is cleared once the Vox wait ends (House4F8657),
        // and the frame that admits it never commits (Main_Tick
        // 0x0055DE4F..0x0055DEC8). A capture that watches the House flag
        // alone sees no victory at all.
        let first = runtime
            .advance_frame(
                &[],
                67,
                TickLane::Ordinary,
                &mut FrameEffects::empty().with_outcome_wait(&mut vox),
            )
            .unwrap();
        assert!(first.outcome_waiting);

        vox.0.set(true);
        let completed = runtime
            .resume_outcome_wait(&mut FrameEffects::empty().with_outcome_wait(&mut vox))
            .unwrap();
        assert!(!completed.outcome_waiting);
        assert!(!runtime.simulation.houses()[&owner].has_won());
        assert!(!runtime.simulation.houses()[&owner].has_lost());

        // The capture routes the result the session admitted.
        assert_eq!(trace_outcome_for(&runtime.simulation, owner), Some("won"));
    }
}
