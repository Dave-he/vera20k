use super::*;

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn new_output_path(tag: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "vera20k-shell-capture-{}-{tag}-{unique}",
        std::process::id()
    ))
}

fn valid_args(output: &Path) -> Vec<OsString> {
    args(&[
        "--shell-capture",
        "main-menu-0xe2-steady",
        "--width",
        "800",
        "--height",
        "600",
        "--cursor-x",
        "400",
        "--cursor-y",
        "300",
        "--output",
        output.to_str().expect("UTF-8 temp path"),
    ])
}

#[test]
fn no_args_preserves_interactive_launch() {
    assert!(matches!(
        parse_launch_args(Vec::<OsString>::new()).expect("parse"),
        AppLaunchMode::Interactive
    ));
}

#[test]
fn strict_capture_args_build_expected_request() {
    let output = new_output_path("valid");
    let launch = parse_launch_args(valid_args(&output)).expect("parse");
    let AppLaunchMode::ShellCapture(request) = launch else {
        panic!("expected capture");
    };
    assert_eq!(
        request.checkpoint(),
        ShellCaptureCheckpoint::MainMenu0xE2Steady
    );
    assert_eq!((request.width(), request.height()), (800, 600));
    assert_eq!((request.cursor_x(), request.cursor_y()), (400, 300));
    assert_eq!(request.output_dir(), output);
}

#[test]
fn live_lan_capture_requires_two_explicit_unicast_endpoints() {
    let output = new_output_path("live-lan");
    let mut values = valid_args(&output);
    values[1] = "lan-live-host".into();
    assert!(parse_launch_args(values.clone()).is_err());
    values.extend(args(&[
        "--lan-bind",
        "127.0.0.1:31000",
        "--lan-peer",
        "127.0.0.1:31010",
    ]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values.clone()).unwrap() else {
        panic!("capture");
    };
    assert_eq!(
        request.live_lan,
        Some(LiveLanConfig {
            transport: LiveLanTransport::Explicit {
                bind: "127.0.0.1:31000".parse().unwrap(),
                peer: "127.0.0.1:31010".parse().unwrap(),
            },
            map: None,
            isolation_seed: None,
            prior_map: None,
            profile_check: false,
            failure_prefix_output: None,
        })
    );
    values[1] = "lan-live-guest".into();
    assert!(parse_launch_args(values.clone()).is_ok());
    let mut isolation = values.clone();
    isolation.extend(args(&["--lan-isolation-seed", "73"]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(isolation.clone()).unwrap() else {
        panic!("capture");
    };
    assert_eq!(request.live_lan.unwrap().isolation_seed, Some(73));
    isolation.extend(args(&["--lan-prior-map", "history.map"]));
    assert!(parse_launch_args(isolation.clone()).is_err());
    isolation[1] = "lan-live-host".into();
    assert!(parse_launch_args(isolation.clone()).is_ok());
    isolation[1] = "lan-battle-guest".into();
    assert!(parse_launch_args(isolation).is_err());
    values[1] = "lan-battle-host".into();
    assert!(parse_launch_args(values.clone()).is_ok());
    values[1] = "lan-battle-guest".into();
    assert!(parse_launch_args(values.clone()).is_ok());
    values[1] = "lan-0xbb-steady".into();
    assert!(parse_launch_args(values).is_err());
}

#[test]
fn production_lan_broadcast_needs_no_socket_overrides() {
    let output = new_output_path("lan-production-broadcast");
    let mut values = valid_args(&output);
    values[1] = "lan-live-host".into();
    values.extend(args(&[
        "--lan-production-broadcast",
        "--lan-map",
        "host-only.map",
    ]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values.clone()).unwrap() else {
        panic!("capture");
    };
    let config = request.live_lan.unwrap();
    assert_eq!(config.transport, LiveLanTransport::ProductionBroadcast);
    assert_eq!(config.map.as_deref(), Some("host-only.map"));
    values.truncate(values.len() - 2);
    values[1] = "lan-live-guest".into();
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values).unwrap() else {
        panic!("capture");
    };
    assert_eq!(
        request.live_lan.unwrap().transport,
        LiveLanTransport::ProductionBroadcast
    );
}

#[test]
fn production_lan_broadcast_rejects_socket_overrides() {
    let output = new_output_path("lan-production-conflict");
    let mut values = valid_args(&output);
    values[1] = "lan-live-host".into();
    values.push("--lan-production-broadcast".into());
    for extra in [
        args(&["--lan-bind", "127.0.0.1:31000"]),
        args(&["--lan-peer", "127.0.0.1:31010"]),
    ] {
        let mut invalid = values.clone();
        invalid.extend(extra);
        assert!(
            parse_launch_args(invalid)
                .unwrap_err()
                .to_string()
                .contains("cannot override sockets")
        );
    }
    values.push("--lan-production-broadcast".into());
    assert!(
        parse_launch_args(values)
            .unwrap_err()
            .to_string()
            .contains("duplicate --lan-production-broadcast")
    );
}

#[test]
fn production_lan_broadcast_requires_normal_full_round_targets() {
    let output = new_output_path("lan-production-target");
    for target in [
        "lan-battle-host",
        "lan-live-timeout-host",
        "lan-live-desync-guest",
        "online-live-host",
        "lan-0xbb-steady",
    ] {
        let mut values = valid_args(&output);
        values[1] = target.into();
        values.push("--lan-production-broadcast".into());
        assert!(
            parse_launch_args(values)
                .unwrap_err()
                .to_string()
                .contains("requires a normal LAN live host or guest"),
            "{target}"
        );
    }
}

#[test]
fn live_lan_host_map_uses_a_logical_name_for_custom_and_generated_rounds() {
    let output = new_output_path("live-lan-map");
    let mut values = valid_args(&output);
    values[1] = "lan-live-host".into();
    values.extend(args(&[
        "--lan-bind",
        "127.0.0.1:31000",
        "--lan-peer",
        "127.0.0.1:31010",
        "--lan-map",
        "host-only.map",
    ]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values.clone()).unwrap() else {
        panic!("capture");
    };
    assert_eq!(
        request.live_lan.unwrap().map.as_deref(),
        Some("host-only.map")
    );
    *values.last_mut().unwrap() = "RandMap.Sed".into();
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values).unwrap() else {
        panic!("capture");
    };
    assert_eq!(
        request.live_lan.unwrap().map.as_deref(),
        Some("RandMap.Sed")
    );
}

#[test]
fn live_lan_map_rejects_guest_targets_and_remote_host_paths() {
    let output = new_output_path("live-lan-map-invalid");
    let mut values = valid_args(&output);
    values[1] = "lan-live-host".into();
    values.extend(args(&[
        "--lan-bind",
        "127.0.0.1:31000",
        "--lan-peer",
        "127.0.0.1:31010",
        "--lan-map",
        "../outside.map",
    ]));
    assert!(parse_launch_args(values.clone()).is_err());
    *values.last_mut().unwrap() = "host-only.map".into();
    values[1] = "lan-live-guest".into();
    assert!(parse_launch_args(values.clone()).is_err());
    values[1] = "lan-battle-host".into();
    assert!(parse_launch_args(values).is_err());
}

#[test]
fn lan_desync_pair_writes_real_prefixes_without_online_credentials() {
    let output = new_output_path("lan-desync-prefix");
    let mut values = valid_args(&output);
    values[1] = "lan-live-desync-host".into();
    values.extend(args(&[
        "--lan-bind",
        "127.0.0.1:31000",
        "--lan-peer",
        "127.0.0.1:31050",
    ]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values.clone()).unwrap() else {
        panic!("capture");
    };
    assert_eq!(
        request.live_lan.unwrap().failure_prefix_output.unwrap(),
        PathBuf::from(format!("{}.prefix.json", output.display()))
    );
    values[1] = "lan-live-desync-guest".into();
    assert!(parse_launch_args(values).is_ok());
}

#[test]
fn live_lan_capture_rejects_overlapping_and_invalid_socket_parameters() {
    let output = new_output_path("live-lan-invalid");
    for (bind, peer) in [
        ("127.0.0.1:65534", "127.0.0.1:31010"),
        ("127.0.0.1:31000", "127.0.0.1:31002"),
        ("127.0.0.1:31000", "0.0.0.0:31010"),
    ] {
        let mut values = valid_args(&output);
        values[1] = "lan-live-host".into();
        values.extend(args(&["--lan-bind", bind, "--lan-peer", peer]));
        assert!(parse_launch_args(values).is_err(), "{bind}/{peer}");
    }
}

#[test]
fn online_guest_capture_requires_a_username_and_an_explicit_host() {
    let output = new_output_path("online-guest");
    let mut values = valid_args(&output);
    values[1] = "online-live-guest".into();
    assert!(parse_launch_args(values.clone()).is_err());
    values.extend(args(&["--online-account", "Peer"]));
    assert!(parse_launch_args(values.clone()).is_err());
    values.extend(args(&["--online-host", "Host"]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values).unwrap() else {
        panic!("capture")
    };
    let config = request.live_online.unwrap();
    assert_eq!(config.account, "Peer");
    assert_eq!(config.host, "Host");
    assert_eq!(config.players, 2);
    assert!(!config.quick_match);
}

/// The watcher is a third account; the host's match sets everything else.
#[test]
fn online_watch_capture_requires_its_own_account_and_the_host() {
    let output = new_output_path("online-watch");
    let mut values = valid_args(&output);
    values[1] = "online-live-watch".into();
    values.extend(args(&["--online-account", "Watcher"]));
    assert!(parse_launch_args(values.clone()).is_err());
    values.extend(args(&["--online-host", "Host"]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values.clone()).unwrap() else {
        panic!("capture")
    };
    let config = request.live_online.unwrap();
    assert_eq!(
        (config.account.as_str(), config.host.as_str()),
        ("Watcher", "Host")
    );
    let mut own = values.clone();
    *own.last_mut().unwrap() = "watcher".into();
    assert!(parse_launch_args(own).is_err());
    values.extend(args(&["--online-frames", "600"]));
    assert!(parse_launch_args(values).is_err());
}

#[test]
fn online_quick_match_capture_leaves_the_match_to_the_matchbot() {
    let output = new_output_path("online-quick-match");
    let mut values = valid_args(&output);
    values[1] = "online-live-guest".into();
    values.extend(args(&[
        "--online-account",
        "Peer",
        "--online-host",
        "Host",
        "--online-quick-match",
    ]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values.clone()).unwrap() else {
        panic!("capture")
    };
    assert!(request.live_online.unwrap().quick_match);
    values.extend(args(&["--online-map", "host-only.map"]));
    assert!(parse_launch_args(values).is_err());
}

#[test]
fn online_capture_rejects_password_and_registration_options() {
    let output = new_output_path("online-username-only");
    let mut values = valid_args(&output);
    values[1] = "online-live-host".into();
    values.extend(args(&["--online-account", "Host"]));
    assert!(parse_launch_args(values.clone()).is_ok());
    values.push("--online-password-stdin".into());
    assert!(parse_launch_args(values.clone()).is_err());
    *values.last_mut().unwrap() = "--online-create-account".into();
    assert!(parse_launch_args(values).is_err());
}

/// A guest looks for "<host>'s_game", which the room name edit cuts at
/// 24 characters.
#[test]
fn online_host_names_leave_room_for_the_room_suffix() {
    let output = new_output_path("online-host-name");
    let mut values = valid_args(&output);
    values[1] = "online-live-coop-guest".into();
    values.extend(args(&[
        "--online-account",
        "Guest",
        "--online-host",
        "seventeen-chars-x",
    ]));
    assert!(parse_launch_args(values.clone()).is_ok());
    *values.last_mut().unwrap() = "eighteen-chars-xyz".into();
    assert!(
        parse_launch_args(values)
            .unwrap_err()
            .to_string()
            .contains("room for 's_game")
    );
}

#[test]
fn online_ai_host_capture_keeps_one_human_and_the_requested_retail_mode() {
    let output = new_output_path("online-ai-host");
    let mut values = valid_args(&output);
    values[1] = "online-live-host".into();
    values.extend(args(&[
        "--online-account",
        "Host",
        "--online-players",
        "1",
        "--online-ai",
        "hard",
        "--online-mode-id",
        "1",
    ]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values).unwrap() else {
        panic!("capture")
    };
    let config = request.live_online.unwrap();
    assert_eq!(config.players, 1);
    assert_eq!(config.ai, Some(crate::skirmish_launch::AiDifficulty::Hard));
    assert_eq!(config.mode_id, Some(1));
    assert_eq!(config.frames, 600);
    assert!(config.simple_input && !config.mass_input);
}

#[test]
fn online_latency_is_opt_in_and_rejects_conflicting_modes() {
    let output = new_output_path("online-latency");
    let mut values = valid_args(&output);
    values[1] = "online-live-host".into();
    values.extend(args(&["--online-account", "Host", "--online-latency"]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values.clone()).unwrap() else {
        panic!("capture");
    };
    assert_eq!((request.width(), request.height()), (800, 600));
    let config = request.live_online.unwrap();
    assert!(config.latency && config.simple_input);
    assert_eq!(config.frames, 6000);
    let mut small = values.clone();
    let width = small.iter().position(|arg| arg == "--width").unwrap();
    let height = small.iter().position(|arg| arg == "--height").unwrap();
    small[width + 1] = "640".into();
    small[height + 1] = "480".into();
    small.extend(args(&["--online-frames", "9000"]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(small.clone()).unwrap() else {
        panic!("capture")
    };
    assert_eq!((request.width(), request.height()), (640, 480));
    assert_eq!(request.live_online.unwrap().frames, 9000);
    small.retain(|arg| arg != "--online-latency");
    assert!(
        parse_launch_args(small).is_err(),
        "ordinary captures retain800x600"
    );
    values.extend(args(&["--online-mass-input"]));
    assert!(parse_launch_args(values).is_err());
}

#[test]
fn online_ai_capture_requires_a_singleton_host_and_actual_play() {
    let output = new_output_path("online-ai-admission");
    let mut values = valid_args(&output);
    values[1] = "online-live-host".into();
    values.extend(args(&["--online-account", "Host", "--online-players", "1"]));
    assert!(parse_launch_args(values.clone()).is_err());
    values.extend(args(&["--online-ai", "normal"]));
    assert!(parse_launch_args(values.clone()).is_ok());
    let mut short = values.clone();
    short.extend(args(&["--online-frames", "300"]));
    assert!(parse_launch_args(short).is_err());
    values[1] = "online-live-guest".into();
    values.extend(args(&["--online-host", "Host"]));
    assert!(parse_launch_args(values).is_err());
}

#[test]
fn online_ai_options_reject_unknown_difficulties_and_nononline_routes() {
    let output = new_output_path("online-ai-invalid");
    let mut values = valid_args(&output);
    values[1] = "online-live-host".into();
    values.extend(args(&[
        "--online-account",
        "Host",
        "--online-players",
        "1",
        "--online-ai",
        "unknown",
    ]));
    assert!(parse_launch_args(values.clone()).is_err());
    *values.last_mut().unwrap() = "easy".into();
    assert!(parse_launch_args(values.clone()).is_ok());
    values[1] = "main-menu-0xe2-steady".into();
    assert!(parse_launch_args(values).is_err());
}

#[test]
fn online_failure_targets_require_a_pair_and_write_a_separate_real_prefix() {
    for target in [
        "online-live-timeout-host",
        "online-live-quit-host",
        "online-live-failure-guest",
    ] {
        let output = new_output_path("online-failure");
        let mut values = valid_args(&output);
        values[1] = target.into();
        values.extend(args(&["--online-account", "Peer", "--online-host", "Host"]));
        let AppLaunchMode::ShellCapture(request) = parse_launch_args(values.clone()).unwrap()
        else {
            panic!("capture")
        };
        let config = request.live_online.unwrap();
        assert_eq!(
            (
                config.players,
                config.frames,
                config.simple_input,
                config.mass_input
            ),
            (2, 300, true, false)
        );
        let mut prefix = output.as_os_str().to_owned();
        prefix.push(".prefix.json");
        assert_eq!(config.failure_prefix_output, Some(PathBuf::from(prefix)));
        values.extend(args(&["--online-players", "3"]));
        assert!(parse_launch_args(values).is_err());
    }
    let output = new_output_path("online-failure-host-required");
    let mut values = valid_args(&output);
    values[1] = "online-live-failure-guest".into();
    values.extend(args(&["--online-account", "Peer"]));
    assert!(parse_launch_args(values).is_err());
}

#[test]
fn scale_capture_uses_explicit_player_and_observation_limits() {
    let output = new_output_path("online-scale");
    let mut values = valid_args(&output);
    values[1] = "online-live-host".into();
    values.extend(args(&[
        "--online-account",
        "Host",
        "--online-players",
        "30",
        "--online-frames",
        "900",
        "--online-mass-input",
        "--online-map",
        "scale.map",
        "--online-start",
        "29",
    ]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values.clone()).unwrap() else {
        panic!("capture")
    };
    let config = request.live_online.unwrap();
    assert_eq!((config.players, config.frames), (30, 900));
    assert!(config.simple_input && config.mass_input);
    assert_eq!(config.map.as_deref(), Some("scale.map"));
    assert_eq!(config.start, Some(29));
    let mut invalid_start = values.clone();
    let start = invalid_start
        .iter()
        .position(|value| value == "--online-start")
        .unwrap();
    invalid_start[start + 1] = "30".into();
    assert!(parse_launch_args(invalid_start).is_err());
    let mut generated = values.clone();
    let map = generated
        .iter()
        .position(|value| value == "scale.map")
        .unwrap();
    generated[map] = "RandMap.Sed".into();
    assert!(
        parse_launch_args(generated).is_err(),
        "generated maps retain their eight-start limit"
    );
    let frames = values.iter().position(|value| value == "900").unwrap();
    values[frames] = "100".into();
    assert!(parse_launch_args(values).is_err());
}

#[test]
fn strict_capture_args_accept_main_menu_entry_sequence() {
    let output = new_output_path("entry-sequence");
    let mut values = valid_args(&output);
    values[1] = OsString::from("main-menu-0xe2-entry-sequence");
    let AppLaunchMode::ShellCapture(request) =
        parse_launch_args(values).expect("entry sequence checkpoint must parse")
    else {
        panic!("expected capture");
    };
    assert_eq!(
        request.checkpoint().as_str(),
        "main-menu-0xe2-entry-sequence"
    );
}

#[test]
fn slide_checkpoints_accept_only_shown_ticks() {
    // Every family slide at 800x600 shows ticks 0..=17 (9 tile rows + 9).
    for (prefix, checkpoint) in [
        (
            "main-menu-0xe2-slide-out-tick-",
            ShellCaptureCheckpoint::MainMenu0xE2SlideOut(17),
        ),
        (
            "movie-list-0x129-slide-out-tick-",
            ShellCaptureCheckpoint::MovieList0x129SlideOut(17),
        ),
        (
            "skirmish-0x102-back-slide-out-tick-",
            ShellCaptureCheckpoint::Skirmish0x102BackSlideOut(17),
        ),
        (
            "skirmish-0x102-entry-tick-",
            ShellCaptureCheckpoint::Skirmish0x102Entry(17),
        ),
        (
            "skirmish-0x105-entry-tick-",
            ShellCaptureCheckpoint::Skirmish0x105Entry(17),
        ),
        (
            "skirmish-0x105-cancel-tick-",
            ShellCaptureCheckpoint::Skirmish0x105CancelEntry(17),
        ),
        (
            "campaign-0x94-entry-tick-",
            ShellCaptureCheckpoint::Campaign0x94Entry(17),
        ),
        (
            "load-saved-game-0xb7-entry-tick-",
            ShellCaptureCheckpoint::LoadSavedGame0xB7Entry(17),
        ),
        (
            "options-0xd5-entry-tick-",
            ShellCaptureCheckpoint::Options0xD5Entry(17),
        ),
    ] {
        assert_eq!(
            ShellCaptureCheckpoint::parse(&format!("{prefix}17")).expect("last tick"),
            checkpoint
        );
        assert!(ShellCaptureCheckpoint::parse(&format!("{prefix}18")).is_err());
    }
}

#[test]
fn skirmish_in_game_menu_checkpoints_round_trip_with_the_game_budget() {
    for (name, target) in SKIRMISH_IN_GAME_MENU_CHECKPOINTS {
        let checkpoint = ShellCaptureCheckpoint::parse(name).expect("in-game menu checkpoint");
        assert_eq!(
            checkpoint,
            ShellCaptureCheckpoint::SkirmishInGameMenu(target)
        );
        assert_eq!(checkpoint.as_str(), name);
    }
    let output = new_output_path("skirmish-pause-menu");
    let mut values = valid_args(&output);
    values[1] = OsString::from("skirmish-pause-sound");
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values).expect("parse") else {
        panic!("expected capture");
    };
    let session = ShellCaptureSession::new(request);
    assert!(session.skirmish.is_some());
    assert_eq!(session.timeout(), Duration::from_secs(180));
}

#[test]
fn options_hover_checkpoints_round_trip_their_names() {
    for (name, ..) in OPTIONS_0XD5_HOVER_CHECKPOINTS {
        let checkpoint = ShellCaptureCheckpoint::parse(name).expect("hover checkpoint");
        assert_eq!(checkpoint.as_str(), name);
    }
    for (name, _) in SKIRMISH_0X102_HOVER_CHECKPOINTS
        .into_iter()
        .chain(SKIRMISH_0X105_HOVER_CHECKPOINTS)
    {
        let checkpoint = ShellCaptureCheckpoint::parse(name).expect("hover checkpoint");
        assert_eq!(checkpoint.as_str(), name);
    }
    assert!(ShellCaptureCheckpoint::parse("options-0xd5-hover-nothing").is_err());
    // Each point rests on the control its name gives.
    use crate::ui::main_menu_dialogs::options::shell::LauncherOptionsLayout;
    use crate::ui::main_menu_dialogs::options::{LauncherCheckboxId, LauncherTrackbarId};
    let [difficulty, tooltips, music] = OPTIONS_0XD5_HOVER_CHECKPOINTS
        .map(|(_, category, (x, y))| (LauncherOptionsLayout::new(800, 600, category), x, y));
    let slider = |(layout, x, y): &(LauncherOptionsLayout, i32, i32), id| {
        layout
            .trackbar_rect(id)
            .is_some_and(|rect| rect.contains(*x, *y))
    };
    assert!(slider(&difficulty, LauncherTrackbarId::Difficulty));
    assert!(slider(&music, LauncherTrackbarId::Score));
    let (layout, x, y) = &tooltips;
    assert!(
        layout
            .checkbox_rect(LauncherCheckboxId::Tooltips)
            .is_some_and(|rect| rect.contains(*x, *y))
    );
    for (name, category) in OPTIONS_SETTINGS_CHECKPOINTS {
        let checkpoint = ShellCaptureCheckpoint::parse(name).expect("settings checkpoint");
        assert_eq!(
            checkpoint,
            ShellCaptureCheckpoint::OptionsSettings(category)
        );
        assert_eq!(checkpoint.as_str(), name);
    }
}

#[test]
fn lan_checkpoints_route_through_production_network() {
    for &(name, target) in movies::LAN_CHECKPOINTS {
        let checkpoint = ShellCaptureCheckpoint::parse(name).expect("LAN checkpoint");
        assert_eq!(checkpoint.as_str(), name);
        assert_eq!(
            checkpoint.movies_target(),
            Some(movies::MoviesTarget::Lan(target))
        );
    }
    assert!(ShellCaptureCheckpoint::parse("lan-0xbd-offline-shortcut").is_err());
}

#[test]
fn wol_checkpoints_route_through_production_wol() {
    for &(name, target) in movies::WOL_CHECKPOINTS {
        let checkpoint = ShellCaptureCheckpoint::parse(name).expect("WOL checkpoint");
        assert_eq!(checkpoint.as_str(), name);
        assert_eq!(
            checkpoint.movies_target(),
            Some(movies::MoviesTarget::Wol(target))
        );
    }
}

#[test]
fn wol_checkpoints_round_trip() {
    let AppLaunchMode::ShellCapture(mut request) =
        parse_launch_args(valid_args(&new_output_path("legacy-wol"))).unwrap()
    else {
        panic!("capture");
    };
    for name in [
        CHECKPOINT_WOL_0X10E_STEADY,
        CHECKPOINT_WOL_0X10E_HOVER_QUICK_MATCH,
        CHECKPOINT_WOL_0X10E_API_MISSING,
        CHECKPOINT_WOL_0X10E_BACK,
        "wol-0x10e-entry-tick-0",
    ] {
        let checkpoint = ShellCaptureCheckpoint::parse(name).expect("WOL checkpoint");
        request.checkpoint = checkpoint;
        assert!(request.native_shell_ui());
        assert_eq!(
            request.wol_welcome_provider(),
            Some(crate::app::shell_wol::WolProvider::Unavailable)
        );
        if !name.contains("entry-tick") {
            assert_eq!(checkpoint.as_str(), name);
        }
    }
    for (name, provider) in [
        (
            "wol-0x113-login-accepted",
            crate::app::shell_wol::WolProvider::Controlled,
        ),
        (
            "wol-0x10e-profile",
            crate::app::shell_wol::WolProvider::Controlled,
        ),
        (
            "wol-test-service-lobby",
            crate::app::shell_wol::WolProvider::TestService,
        ),
    ] {
        request.checkpoint = ShellCaptureCheckpoint::parse(name).unwrap();
        assert!(request.native_shell_ui());
        assert_eq!(request.wol_welcome_provider(), Some(provider));
    }
    assert_eq!(
        ShellCaptureCheckpoint::parse("wol-0x10e-entry-tick-17").expect("last tick"),
        ShellCaptureCheckpoint::Wol0x10EEntry(17)
    );
    assert!(ShellCaptureCheckpoint::parse("wol-0x10e-entry-tick-18").is_err());
}

#[test]
fn modern_wol_captures_require_no_login_inputs() {
    let output = new_output_path("modern-wol");
    let mut values = valid_args(&output);
    values[1] = "wol-modern-username".into();
    let AppLaunchMode::ShellCapture(mut request) = parse_launch_args(values).expect("parse") else {
        panic!("capture");
    };
    assert!(request.live_online.is_none());
    request.checkpoint = ShellCaptureCheckpoint::Lan(movies::LanTarget::OnlineHost);
    assert!(!request.native_shell_ui());
    assert_eq!(request.wol_welcome_provider(), None);
    request.checkpoint = ShellCaptureCheckpoint::Lan(movies::LanTarget::Accepted);
    assert!(request.native_shell_ui(), "BD diagnostics keep Accept");
    request.checkpoint = ShellCaptureCheckpoint::parse("wol-modern-lobby").unwrap();
    assert!(!request.native_shell_ui());
    assert!(request.live_online.is_none());
    assert_eq!(
        request.wol_welcome_provider(),
        Some(crate::app::shell_wol::WolProvider::TestService)
    );
    request.checkpoint = ShellCaptureCheckpoint::parse("wol-modern-create").unwrap();
    assert!(!request.native_shell_ui());
    request.checkpoint = ShellCaptureCheckpoint::parse("wol-modern-username").unwrap();
    assert!(!request.native_shell_ui());
    assert_eq!(
        request.wol_welcome_provider(),
        Some(crate::app::shell_wol::WolProvider::Online)
    );
}

#[test]
fn campaign_start_checkpoints_select_only_the_normal_route_targets() {
    use crate::ui::campaign_shell::CampaignSide::{Allied, Soviet};
    for (name, target) in [
        (
            "campaign-all1-initial-save",
            movies::MoviesTarget::CampaignStart {
                side: Allied,
                target: movies::CampaignStartTarget::InitialSaved,
            },
        ),
        (
            "campaign-sov1-initial-save",
            movies::MoviesTarget::CampaignStart {
                side: Soviet,
                target: movies::CampaignStartTarget::InitialSaved,
            },
        ),
        (
            "campaign-all02-first-live-frame",
            movies::MoviesTarget::CampaignStart {
                side: Allied,
                target: movies::CampaignStartTarget::ExplicitFirstLive("ALL02UMD.MAP"),
            },
        ),
        (
            "campaign-sov05-first-live-frame",
            movies::MoviesTarget::CampaignStart {
                side: Soviet,
                target: movies::CampaignStartTarget::ExplicitFirstLive("SOV05UMD.MAP"),
            },
        ),
        (
            "campaign-sov07-first-live-frame",
            movies::MoviesTarget::CampaignStart {
                side: Soviet,
                target: movies::CampaignStartTarget::ExplicitFirstLive("SOV07TMD.MAP"),
            },
        ),
        (
            CHECKPOINT_CAMPAIGN_ALL1_LOADING_FIRST,
            movies::MoviesTarget::CampaignStart {
                side: Allied,
                target: movies::CampaignStartTarget::LoadingFirst,
            },
        ),
        (
            CHECKPOINT_CAMPAIGN_SOV1_LOADING_FIRST,
            movies::MoviesTarget::CampaignStart {
                side: Soviet,
                target: movies::CampaignStartTarget::LoadingFirst,
            },
        ),
        (
            CHECKPOINT_CAMPAIGN_ALL1_FIRST_LIVE,
            movies::MoviesTarget::CampaignStart {
                side: Allied,
                target: movies::CampaignStartTarget::FirstLive,
            },
        ),
        (
            CHECKPOINT_CAMPAIGN_SOV1_FIRST_LIVE,
            movies::MoviesTarget::CampaignStart {
                side: Soviet,
                target: movies::CampaignStartTarget::FirstLive,
            },
        ),
        (
            "campaign-sov02-objective-expiry",
            movies::MoviesTarget::CampaignStart {
                side: Soviet,
                target: movies::CampaignStartTarget::Restart(
                    movies::RestartTarget::ObjectiveExpiry,
                ),
            },
        ),
        (
            "campaign-sov6-alternate-home-victory",
            movies::MoviesTarget::CampaignStart {
                side: Soviet,
                target: movies::CampaignStartTarget::Restart(
                    movies::RestartTarget::AlternateHomeVictory,
                ),
            },
        ),
        (
            CHECKPOINT_CAMPAIGN_ALL1_ABORT_RETURN,
            movies::MoviesTarget::CampaignStart {
                side: Allied,
                target: movies::CampaignStartTarget::AbortReturn,
            },
        ),
        (
            CHECKPOINT_CAMPAIGN_SOV1_ABORT_RETURN,
            movies::MoviesTarget::CampaignStart {
                side: Soviet,
                target: movies::CampaignStartTarget::AbortReturn,
            },
        ),
        (
            CHECKPOINT_CAMPAIGN_ALL1_ABORT_RESTART_SHOWN,
            movies::MoviesTarget::CampaignStart {
                side: Allied,
                target: movies::CampaignStartTarget::Restart(movies::RestartTarget::AbortShown),
            },
        ),
        (
            CHECKPOINT_CAMPAIGN_ALL1_ABORT_RESTART,
            movies::MoviesTarget::CampaignStart {
                side: Allied,
                target: movies::CampaignStartTarget::Restart(movies::RestartTarget::AbortRestarted),
            },
        ),
        (
            CHECKPOINT_CAMPAIGN_ALL1_DEFEAT_REPLAY_PROMPT,
            movies::MoviesTarget::CampaignStart {
                side: Allied,
                target: movies::CampaignStartTarget::Restart(movies::RestartTarget::DefeatPrompt),
            },
        ),
        (
            CHECKPOINT_CAMPAIGN_ALL1_DEFEAT_REPLAY_NO,
            movies::MoviesTarget::CampaignStart {
                side: Allied,
                target: movies::CampaignStartTarget::Restart(movies::RestartTarget::DefeatReturned),
            },
        ),
        (
            CHECKPOINT_CAMPAIGN_ALL1_DEFEAT_REPLAY_YES,
            movies::MoviesTarget::CampaignStart {
                side: Allied,
                target: movies::CampaignStartTarget::Restart(movies::RestartTarget::DefeatReplayed),
            },
        ),
        (
            "campaign-all1-defeat-replay-enter",
            movies::MoviesTarget::CampaignStart {
                side: Allied,
                target: movies::CampaignStartTarget::Restart(
                    movies::RestartTarget::DefeatReturnedEnter,
                ),
            },
        ),
        (
            "campaign-all1-defeat-replay-numpad-enter",
            movies::MoviesTarget::CampaignStart {
                side: Allied,
                target: movies::CampaignStartTarget::Restart(
                    movies::RestartTarget::DefeatReturnedNumpadEnter,
                ),
            },
        ),
        (
            "campaign-all1-defeat-replay-escape",
            movies::MoviesTarget::CampaignStart {
                side: Allied,
                target: movies::CampaignStartTarget::Restart(
                    movies::RestartTarget::DefeatReturnedEscape,
                ),
            },
        ),
        (
            "campaign-all1-defeat-replay-repeated-enter",
            movies::MoviesTarget::CampaignStart {
                side: Allied,
                target: movies::CampaignStartTarget::Restart(
                    movies::RestartTarget::DefeatReturnedRepeatedEnter,
                ),
            },
        ),
        (
            CHECKPOINT_CAMPAIGN_ALL1_RESTATE,
            movies::MoviesTarget::CampaignStart {
                side: Allied,
                target: movies::CampaignStartTarget::Restart(movies::RestartTarget::Restate),
            },
        ),
        (
            CHECKPOINT_CAMPAIGN_BACK_RETURN,
            movies::MoviesTarget::CampaignBack,
        ),
    ] {
        let checkpoint = ShellCaptureCheckpoint::parse(name).expect("campaign checkpoint");
        assert_eq!(checkpoint.as_str(), name);
        assert_eq!(checkpoint.movies_target(), Some(target));
    }
    for (name, target, side) in [
        (
            CHECKPOINT_CAMPAIGN_SOV1_OPENING_LOCKED,
            movies::OpeningTarget::Locked,
            Soviet,
        ),
        (
            CHECKPOINT_CAMPAIGN_SOV1_OPENING_PAN,
            movies::OpeningTarget::CameraPan,
            Soviet,
        ),
        (
            CHECKPOINT_CAMPAIGN_SOV1_OPENING_FLASH,
            movies::OpeningTarget::Flash,
            Soviet,
        ),
        (
            CHECKPOINT_CAMPAIGN_SOV1_OPENING_ARROWS,
            movies::OpeningTarget::Arrows,
            Soviet,
        ),
        (
            CHECKPOINT_CAMPAIGN_SOV1_OPENING_UNLOCKED,
            movies::OpeningTarget::Unlocked,
            Soviet,
        ),
        (
            "campaign-sov1-opening-timer-running",
            movies::OpeningTarget::TimerRunning,
            Soviet,
        ),
        (
            "campaign-sov1-opening-timer-expired",
            movies::OpeningTarget::TimerExpired,
            Soviet,
        ),
        (
            "campaign-sov2-radar-alert",
            movies::OpeningTarget::RadarAlert,
            Soviet,
        ),
        (
            "campaign-sov2-radar-restored",
            movies::OpeningTarget::RadarRestored,
            Soviet,
        ),
        (
            CHECKPOINT_CAMPAIGN_SOV1_OPENING_RESTORE_UNLOCKED,
            movies::OpeningTarget::RestoreUnlocked,
            Soviet,
        ),
        (
            CHECKPOINT_CAMPAIGN_ALL1_OPENING_LOAD_UNLOCKED,
            movies::OpeningTarget::LoadUnlockedWhileLocked,
            Allied,
        ),
    ] {
        let checkpoint = ShellCaptureCheckpoint::parse(name).expect("opening checkpoint");
        assert_eq!(checkpoint.as_str(), name);
        assert_eq!(
            checkpoint.movies_target(),
            Some(movies::MoviesTarget::CampaignStart {
                side,
                target: movies::CampaignStartTarget::Opening(target),
            })
        );
    }
    for name in [
        "campaign-tut1-first-live-frame",
        "campaign-all1-loading-later-frame",
    ] {
        assert!(ShellCaptureCheckpoint::parse(name).is_err());
    }
}

#[test]
fn allied_opening_tick_capture_keeps_its_production_frame_and_side() {
    use crate::ui::campaign_shell::CampaignSide;
    let checkpoint = ShellCaptureCheckpoint::parse("campaign-all1-opening-tick-180")
        .expect("fixed opening frame");
    assert_eq!(
        checkpoint.movies_target(),
        Some(movies::MoviesTarget::CampaignStart {
            side: CampaignSide::Allied,
            target: movies::CampaignStartTarget::Opening(movies::OpeningTarget::AlliedTick(180)),
        })
    );
    for value in ["0", "2501", "-1", "not-a-tick"] {
        assert!(
            ShellCaptureCheckpoint::parse(&format!("campaign-all1-opening-tick-{value}")).is_err()
        );
    }
}

#[test]
fn soviet_encounter_captures_select_the_real_campaign_and_bound_play_inputs() {
    use crate::ui::campaign_shell::CampaignSide;
    for (name, target) in [
        (
            "campaign-sov1-opening-tick-1500",
            movies::OpeningTarget::SovietTick(1500),
        ),
        (
            "campaign-sov1-navy-tick-6500",
            movies::OpeningTarget::SovietNavy(6500),
        ),
        (
            "campaign-sov07-groups-tick-8000",
            movies::OpeningTarget::SovietGroups(8000),
        ),
        (
            "campaign-sov02-fortresses-tick-8000",
            movies::OpeningTarget::SovietFortresses(8000),
        ),
    ] {
        assert_eq!(
            ShellCaptureCheckpoint::parse(name).unwrap().movies_target(),
            Some(movies::MoviesTarget::CampaignStart {
                side: CampaignSide::Soviet,
                target: movies::CampaignStartTarget::Opening(target),
            })
        );
    }
    for invalid in [
        "campaign-sov1-opening-tick-0",
        "campaign-sov1-navy-tick-1099",
        "campaign-sov1-navy-tick-10001",
        "campaign-sov1-navy-tick-not-a-tick",
        "campaign-sov07-groups-tick-300",
        "campaign-sov07-groups-tick-10001",
        "campaign-sov02-fortresses-tick-300",
        "campaign-sov02-fortresses-tick-10001",
    ] {
        assert!(ShellCaptureCheckpoint::parse(invalid).is_err());
    }
}

#[test]
fn trigger_trace_checkpoint_selects_the_named_retail_mission_and_its_side() {
    use crate::ui::campaign_shell::CampaignSide;
    let capture_cells: movies::TraceCaptureCells = [
        (125, 83),
        (130, 129),
        (0, 0),
        (0, 0),
        (0, 0),
        (0, 0),
        (0, 0),
        (0, 0),
    ];
    assert_eq!(movies::MAX_TRACE_CAPTURE_CELLS, capture_cells.len());
    let leg_cells: movies::TraceLegs = [
        (300, (90, 107)),
        (1500, (48, 89)),
        (0, (0, 0)),
        (0, (0, 0)),
        (0, (0, 0)),
        (0, (0, 0)),
        (0, (0, 0)),
        (0, (0, 0)),
    ];
    assert_eq!(movies::MAX_TRACE_LEGS, leg_cells.len());
    let mut train_cells = movies::TraceTrain::default();
    assert!(train_cells.push(movies::TraceTypeName::new("ENGINEER").unwrap(), 6));
    for (name, side, scenario, tick, orders) in [
        (
            "campaign-trigger-trace-all04dmd-tick-9000",
            CampaignSide::Allied,
            "ALL04DMD.MAP",
            9000,
            movies::TraceOrders::default(),
        ),
        (
            "campaign-trigger-trace-SOV07TMD-tick-1",
            CampaignSide::Soviet,
            "SOV07TMD.MAP",
            1,
            movies::TraceOrders::default(),
        ),
        (
            "campaign-trigger-trace-ALL02UMD-tick-18000-push-83-88",
            CampaignSide::Allied,
            "ALL02UMD.MAP",
            18000,
            movies::TraceOrders {
                push: Some((83, 88)),
                ..movies::TraceOrders::default()
            },
        ),
        (
            "campaign-trigger-trace-ALL01UMD-tick-18000-push-125-83-capture-125-83,130-129",
            CampaignSide::Allied,
            "ALL01UMD.MAP",
            18000,
            movies::TraceOrders {
                push: Some((125, 83)),
                capture: Some((capture_cells, 2)),
                ..movies::TraceOrders::default()
            },
        ),
        (
            // The two segments may come in either order.
            "campaign-trigger-trace-ALL01UMD-tick-18000-capture-125-83,130-129-push-91-105",
            CampaignSide::Allied,
            "ALL01UMD.MAP",
            18000,
            movies::TraceOrders {
                push: Some((91, 105)),
                capture: Some((capture_cells, 2)),
                ..movies::TraceOrders::default()
            },
        ),
        (
            // The legs keep the tick order, whatever order they are named in.
            "campaign-trigger-trace-ALL03UMD-tick-18000-legs-1500-48-89,300-90-107",
            CampaignSide::Allied,
            "ALL03UMD.MAP",
            18000,
            movies::TraceOrders {
                legs: Some((leg_cells, 2)),
                ..movies::TraceOrders::default()
            },
        ),
        (
            // The trains and the capture delay are player orders too.
            "campaign-trigger-trace-ALL01UMD-tick-18000-train-ENGINEER-6-capture-after-9000",
            CampaignSide::Allied,
            "ALL01UMD.MAP",
            18000,
            movies::TraceOrders {
                train: train_cells,
                capture_after: Some(9000),
                ..movies::TraceOrders::default()
            },
        ),
        (
            // The screenshot interval is a player order as well: it names
            // the pictures the run leaves of the same battle.
            "campaign-trigger-trace-ALL01UMD-tick-18000-film-300",
            CampaignSide::Allied,
            "ALL01UMD.MAP",
            18000,
            movies::TraceOrders {
                film_every: Some(300),
                ..movies::TraceOrders::default()
            },
        ),
    ] {
        let target = movies::OpeningTarget::TriggerTrace {
            scenario,
            tick,
            orders,
        };
        assert_eq!(
            ShellCaptureCheckpoint::parse(name).unwrap().movies_target(),
            Some(movies::MoviesTarget::CampaignStart {
                side,
                target: movies::CampaignStartTarget::Opening(target),
            })
        );
    }
    for invalid in [
        "campaign-trigger-trace-ALL08UMD-tick-100",
        "campaign-trigger-trace-ALL01UMD-tick-0",
        "campaign-trigger-trace-ALL01UMD-tick-30001",
        "campaign-trigger-trace-ALL01UMD",
        "campaign-trigger-trace-ALL01UMD-tick-100-push-5",
        "campaign-trigger-trace-ALL01UMD-tick-100-capture-125",
        "campaign-trigger-trace-ALL01UMD-tick-100-capture-125-65536",
        "campaign-trigger-trace-ALL01UMD-tick-100-capture-1-1,2-2,3-3,4-4,5-5,6-6,7-7,8-8,9-9",
        "campaign-trigger-trace-ALL01UMD-tick-100-push--1-5",
        "campaign-trigger-trace-ALL01UMD-tick-100-push-capture-1-1",
        "campaign-trigger-trace-ALL01UMD-tick-100-legs-abc-1-1",
        "campaign-trigger-trace-ALL01UMD-tick-100-legs-100-1",
        "campaign-trigger-trace-ALL01UMD-tick-100-legs-100-1-1,200-2-2,300-3-3,400-4-4,500-5-5,600-6-6,700-7-7,800-8-8,900-9-9",
        "campaign-trigger-trace-ALL01UMD-tick-100-capture-push-1-1",
        "campaign-trigger-trace-ALL01UMD-tick-100-train-ENGINEER",
        "campaign-trigger-trace-ALL01UMD-tick-100-train-ENGINEER-0",
        "campaign-trigger-trace-ALL01UMD-tick-100-train-ENGINEER-many",
        "campaign-trigger-trace-ALL01UMD-tick-100-train-A-VERY-LONG-UNIT-TYPE-NAME-1",
        "campaign-trigger-trace-ALL01UMD-tick-100-capture-after-abc",
        "campaign-trigger-trace-ALL01UMD-tick-100-film-abc",
        "campaign-trigger-trace-ALL01UMD-tick-100-film-0",
    ] {
        assert!(ShellCaptureCheckpoint::parse(invalid).is_err());
    }
    // The capture list holds exactly as many cells as the probe may name.
    let eight: movies::TraceCaptureCells = [
        (1, 1),
        (2, 2),
        (3, 3),
        (4, 4),
        (5, 5),
        (6, 6),
        (7, 7),
        (8, 8),
    ];
    assert_eq!(
        ShellCaptureCheckpoint::parse(
            "campaign-trigger-trace-ALL01UMD-tick-100-capture-1-1,2-2,3-3,4-4,5-5,6-6,7-7,8-8",
        )
        .unwrap()
        .movies_target(),
        Some(movies::MoviesTarget::CampaignStart {
            side: CampaignSide::Allied,
            target: movies::CampaignStartTarget::Opening(movies::OpeningTarget::TriggerTrace {
                scenario: "ALL01UMD.MAP",
                tick: 100,
                orders: movies::TraceOrders {
                    capture: Some((eight, 8)),
                    ..movies::TraceOrders::default()
                },
            },),
        })
    );
}

#[test]
fn network_bounce_checkpoint_round_trips() {
    let checkpoint = ShellCaptureCheckpoint::parse(CHECKPOINT_MAIN_MENU_0XE2_NETWORK_BOUNCE)
        .expect("network bounce checkpoint");
    assert_eq!(
        checkpoint,
        ShellCaptureCheckpoint::MainMenu0xE2NetworkBounce
    );
    assert_eq!(
        checkpoint.as_str(),
        CHECKPOINT_MAIN_MENU_0XE2_NETWORK_BOUNCE
    );
}

#[test]
fn entry_sequence_holds_every_main_menu_entry_tick() {
    let rows =
        crate::ui::shell::geom::right_panel_rects(EXPECTED_WIDTH as i32, EXPECTED_HEIGHT as i32)
            .tile_count as u32;
    assert_eq!(
        ShellSlideKind::MainMenu.column(rows, None).total_ticks(),
        u32::from(ENTRY_SEQUENCE_FRAMES)
    );
}

#[test]
fn skirmish_capture_uses_distinct_checkpoint_and_route_budget() {
    let output = new_output_path("skirmish");
    let mut values = valid_args(&output);
    values[1] = OsString::from(CHECKPOINT_SKIRMISH_0X102_STEADY);
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values).expect("parse") else {
        panic!("expected capture");
    };
    let session = ShellCaptureSession::new(request);
    assert_eq!(
        session.request().checkpoint(),
        ShellCaptureCheckpoint::Skirmish0x102Steady
    );
    assert_eq!(session.timeout(), Duration::from_secs(60));
    assert!(!session.is_entry_sequence());
    assert!(session.skirmish.is_some());
}

#[test]
fn capture_timeout_requires_one_positive_capture_only_value() {
    let output = new_output_path("timeout-args");
    let mut zero = valid_args(&output);
    zero.extend(args(&["--shell-capture-timeout-seconds", "0"]));
    assert!(
        parse_launch_args(zero)
            .unwrap_err()
            .to_string()
            .contains("must be greater than zero")
    );

    let mut duplicate = valid_args(&output);
    duplicate.extend(args(&[
        "--shell-capture-timeout-seconds",
        "3500",
        "--shell-capture-timeout-seconds",
        "3600",
    ]));
    assert!(
        parse_launch_args(duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicate shell-capture option --shell-capture-timeout-seconds")
    );
    assert!(parse_launch_args(args(&["--shell-capture-timeout-seconds", "3500"])).is_err());
}

#[test]
fn latency_capture_uses_immediate_redraws_but_keeps_its_wall_deadline() {
    let output = new_output_path("latency-cadence");
    let mut values = valid_args(&output);
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values.clone()).unwrap() else {
        panic!("capture")
    };
    let mut still = ShellCaptureSession::new(request);
    let now = Instant::now();
    assert_eq!(
        still.next_wake_deadline_at(now),
        now + Duration::from_millis(8)
    );
    still.frames_seen = still.max_capture_frames() + 1;
    assert!(still.check_capture_budget(Duration::ZERO).is_err());

    values[1] = "online-live-host".into();
    values.extend(args(&[
        "--online-account",
        "Host",
        "--online-latency",
        "--online-frames",
        "9000",
    ]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values).unwrap() else {
        panic!("capture")
    };
    let mut latency = ShellCaptureSession::new(request);
    assert_eq!(latency.next_wake_deadline_at(now), now);
    latency.frames_seen = u32::MAX;
    assert!(latency.check_capture_budget(latency.timeout()).is_ok());
    assert!(
        latency
            .check_capture_budget(latency.timeout() + Duration::from_nanos(1))
            .is_err()
    );
}

#[test]
fn live_network_frame_budget_covers_two_rounds_and_keeps_stills_bounded() {
    let output = new_output_path("live-frame-budget");
    let values = valid_args(&output);
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values.clone()).unwrap() else {
        panic!("capture");
    };
    assert_eq!(
        ShellCaptureSession::new(request).max_capture_frames(),
        10_000
    );

    let mut online = values.clone();
    online[1] = "online-live-host".into();
    online.extend(args(&["--online-account", "Host"]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(online.clone()).unwrap() else {
        panic!("capture");
    };
    let two_rounds = 2 * request.live_online.as_ref().unwrap().frames;
    let session = ShellCaptureSession::new(request);
    assert_eq!(two_rounds, 14_400);
    assert!(session.max_capture_frames() > two_rounds + 10_000);
    assert_eq!(session.max_capture_frames(), 310_000);
    assert_eq!(session.timeout(), Duration::from_secs(2400));

    let mut lan = values;
    lan[1] = "lan-live-host".into();
    lan.extend(args(&[
        "--lan-bind",
        "127.0.0.1:31000",
        "--lan-peer",
        "127.0.0.1:31010",
    ]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(lan).unwrap() else {
        panic!("capture");
    };
    assert_eq!(
        ShellCaptureSession::new(request).max_capture_frames(),
        310_000
    );

    online.extend(args(&["--shell-capture-timeout-seconds", "3500"]));
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(online).unwrap() else {
        panic!("capture");
    };
    let mut session = ShellCaptureSession::new(request);
    assert_eq!(session.timeout(), Duration::from_secs(3500));
    assert_eq!(session.max_capture_frames(), 447_500);
    session.started_at = Some(Instant::now());
    let remaining = session
        .readback_timeout()
        .expect("override reaches readback");
    assert!(remaining > Duration::from_secs(2400));
    assert!(remaining <= Duration::from_secs(3500));
}

#[test]
fn campaign_score_capture_selects_the_synthetic_0x103_route() {
    let output = new_output_path("campaign-score");
    let mut values = valid_args(&output);
    values[1] = OsString::from(CHECKPOINT_CAMPAIGN_SCORE_0X103_STEADY);
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values).expect("parse") else {
        panic!("expected capture");
    };
    assert_eq!(
        request.checkpoint(),
        ShellCaptureCheckpoint::CampaignScore0x103Steady
    );
    assert_eq!(
        ShellCaptureCheckpoint::parse(CHECKPOINT_CAMPAIGN_SCORE_0X103_STEADY)
            .expect("checkpoint")
            .as_str(),
        CHECKPOINT_CAMPAIGN_SCORE_0X103_STEADY
    );
    let session = ShellCaptureSession::new(request);
    assert_eq!(session.timeout(), Duration::from_secs(60));
    assert!(session.campaign_score.is_some());
    assert!(session.score.is_none());
}

#[test]
fn entry_sequence_manifest_has_exact_ticks_offsets_and_presenter_domain() {
    let output = new_output_path("entry-manifest");
    let mut values = valid_args(&output);
    values[1] = OsString::from(CHECKPOINT_MAIN_MENU_0XE2_ENTRY_SEQUENCE);
    let AppLaunchMode::ShellCapture(request) = parse_launch_args(values).expect("parse") else {
        panic!("expected capture");
    };
    let value = serde_json::to_value(entry_sequence_manifest(
        &request,
        wgpu::TextureFormat::Bgra8UnormSrgb,
    ))
    .expect("serialize");
    assert_eq!(
        value["schema_version"].as_str(),
        Some("vera20k.shell-entry-sequence-capture.v1")
    );
    assert_eq!(
        value["presenter_domain"].as_str(),
        Some("final-swapchain-after-rgb565")
    );
    assert_eq!(value["generation"].as_u64(), Some(1));
    assert_eq!(value["completion_observed"].as_bool(), Some(true));
    // 18 ticks of 800x600 BGRA8 (9 tile rows + 9, `0x006071E0`).
    assert_eq!(value["payload"]["byte_length"].as_u64(), Some(34_560_000));
    let frames = value["frames"].as_array().expect("frames");
    assert_eq!(frames.len(), 18);
    for (tick, frame) in frames.iter().enumerate() {
        assert_eq!(frame["tick"].as_u64(), Some(tick as u64));
        assert_eq!(frame["byte_offset"].as_u64(), Some(tick as u64 * 1_920_000));
        assert_eq!(frame["byte_length"].as_u64(), Some(1_920_000));
    }
}

#[test]
fn entry_sequence_ledger_rejects_gap_and_duplicate() {
    let mut sequence = EntrySequenceState::default();
    let first = EntrySequenceFrameIdentity { tick: 0 };
    sequence.validate_next(first).expect("first");
    sequence.expected_next_tick = 1;
    assert!(sequence.validate_next(first).is_err());
    assert!(
        sequence
            .validate_next(EntrySequenceFrameIdentity { tick: 2 })
            .is_err()
    );
}

#[test]
fn capture_manifest_serializes_the_strict_validator_schema() {
    let output = new_output_path("manifest");
    let launch = parse_launch_args(valid_args(&output)).expect("parse");
    let AppLaunchMode::ShellCapture(request) = launch else {
        panic!("expected capture");
    };
    let value = serde_json::to_value(capture_manifest(
        &request,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        800 * 600 * 4,
    ))
    .expect("serialize");

    assert_eq!(
        value["schema_version"].as_str(),
        Some("vera20k.shell-capture.v2")
    );
    assert_eq!(value["checkpoint"].as_str(), Some("main-menu-0xe2-steady"));
    assert_eq!(value["surface"]["width"].as_u64(), Some(800));
    assert_eq!(value["surface"]["height"].as_u64(), Some(600));
    assert_eq!(value["surface"]["format"].as_str(), Some("Bgra8UnormSrgb"));
    assert_eq!(value["surface"]["pixel_layout"].as_str(), Some("BGRA8"));
    assert_eq!(value["surface"]["row_order"].as_str(), Some("top-left"));
    assert_eq!(value["surface"]["bytes_per_pixel"].as_u64(), Some(4));
    assert_eq!(value["surface"]["row_stride"].as_u64(), Some(3200));
    assert_eq!(value["cursor"]["x"].as_u64(), Some(400));
    assert_eq!(value["cursor"]["y"].as_u64(), Some(300));
    assert_eq!(
        value["cursor"]["policy"].as_str(),
        Some("software-composited")
    );
    assert_eq!(value["shell"]["screen"].as_str(), Some("main-menu"));
    assert_eq!(value["shell"]["dialog_resource_id"].as_u64(), Some(0x00E2));
    assert_eq!(
        value["shell"]["movie_owner"].as_str(),
        Some("main-menu-0xe2")
    );
    assert_eq!(value["shell"]["movie_base"].as_str(), Some("ra2ts-l"));
    assert_eq!(
        value["shell"]["title_terminal_persistent"].as_bool(),
        Some(true)
    );
    assert_eq!(value["frame"]["path"].as_str(), Some("frame.bgra"));
    assert_eq!(value["frame"]["byte_length"].as_u64(), Some(1_920_000));
}

#[test]
fn unsupported_resolution_fails_closed() {
    let output = new_output_path("resolution");
    let mut values = valid_args(&output);
    let width = values
        .iter()
        .position(|value| value == "--width")
        .expect("width");
    values[width + 1] = OsString::from("600");
    let err = parse_launch_args(values).expect_err("must reject");
    assert!(err.to_string().contains("requires exactly 800x600"));
}

#[test]
fn a_scaled_main_menu_capture_rests_the_pointer_at_the_window_centre() {
    let set = |values: &mut Vec<OsString>, flag: &str, value: &str| {
        let at = values.iter().position(|v| v == flag).expect("flag");
        values[at + 1] = OsString::from(value);
    };
    let output = new_output_path("scaled");
    let mut values = valid_args(&output);
    set(&mut values, "--width", "1440");
    set(&mut values, "--height", "1080");
    let err = parse_launch_args(values.clone()).expect_err("off-centre pointer");
    assert!(
        err.to_string()
            .contains("requires neutral cursor (720,540)")
    );
    set(&mut values, "--cursor-x", "720");
    set(&mut values, "--cursor-y", "540");
    assert!(parse_launch_args(values).is_ok());
}

#[test]
fn duplicate_option_fails_closed() {
    let output = new_output_path("duplicate");
    let mut values = valid_args(&output);
    values.extend(args(&["--width", "800"]));
    let err = parse_launch_args(values).expect_err("must reject");
    assert!(err.to_string().contains("duplicate"));
}

#[test]
fn existing_output_directory_is_never_overwritten() {
    let output = new_output_path("existing");
    fs::create_dir(&output).expect("create owned temp dir");
    let err = parse_launch_args(valid_args(&output)).expect_err("must reject");
    assert!(err.to_string().contains("already exists"));
    fs::remove_dir(&output).expect("remove owned temp dir");
}

fn ready_snapshot() -> MainMenuCaptureSnapshot {
    MainMenuCaptureSnapshot {
        width: 800,
        height: 600,
        main_menu_screen: true,
        shell_failed: false,
        single_player_active: false,
        skirmish_active: false,
        legacy_skirmish_setup_active: false,
        modal_open: false,
        quit_active: false,
        first_paint_slide_active: false,
        active_slide_is_main_menu: true,
        title_terminal_persistent: true,
        movie_loaded: true,
        movie_owner_is_main_menu: true,
        movie_base_is_large: true,
        movie_base_matches_layout: true,
        chrome_loaded: true,
        software_cursor_active: true,
        cursor_x: 400.0,
        cursor_y: 300.0,
    }
}

#[test]
fn active_wave_waits_without_weakening_identity_checks() {
    let mut snapshot = ready_snapshot();
    snapshot.first_paint_slide_active = true;
    snapshot.movie_loaded = false;
    snapshot.movie_owner_is_main_menu = false;
    assert!(!steady_main_menu_capture_ready(snapshot).expect("wait"));
}

#[test]
fn first_ordinary_steady_frame_is_capture_ready() {
    assert!(steady_main_menu_capture_ready(ready_snapshot()).expect("ready"));
}

#[test]
fn latency_main_menu_entry_and_return_accept_the_640_shell_movie() {
    let mut snapshot = ready_snapshot();
    snapshot.width = 640;
    snapshot.height = 480;
    snapshot.movie_base_is_large = false;
    assert!(steady_main_menu_capture_ready_for(snapshot, true).unwrap());
    assert!(
        steady_main_menu_capture_ready(snapshot).is_err(),
        "ordinary reference captures remain fixed"
    );
    snapshot.movie_base_matches_layout = false;
    assert!(
        steady_main_menu_capture_ready_for(snapshot, true).is_err(),
        "the actual 640 shell movie identity is still required"
    );
}

#[test]
fn running_title_waits_for_the_retained_terminal_frame() {
    let mut snapshot = ready_snapshot();
    snapshot.title_terminal_persistent = false;
    assert!(!steady_main_menu_capture_ready(snapshot).expect("wait"));
}

#[test]
fn wrong_movie_owner_is_invalid_not_waiting() {
    let mut snapshot = ready_snapshot();
    snapshot.movie_owner_is_main_menu = false;
    let err = steady_main_menu_capture_ready(snapshot).expect_err("must reject");
    assert!(err.to_string().contains("session identity"));
}
