use super::*;
use super::pipewire::{parse_event, parse_flatpak_info, parse_streams, resolve_game};
use std::collections::HashMap;

const GAME: u32 = 1000;
const OTHER_GAME: u32 = 2000;

fn stream(index: u32, game: Option<u32>, app: &str, muted: bool) -> Stream {
    Stream { index, game, app: app.to_string(), muted }
}

/// A game with one audio stream, plus a browser that must never be touched.
fn scene(game_muted: bool) -> Vec<Stream> {
    vec![
        stream(10, Some(GAME), "Hades", game_muted),
        stream(20, None, "Chromium", false),
    ]
}

fn on() -> Muter {
    Muter::new(true, BTreeSet::new())
}

/// Focus the game, then leave it — the move the whole feature is about.
fn backgrounded(m: &mut Muter) -> Vec<Mute> {
    m.step(Input::Focus { game: Some(GAME) }, &scene(false));
    m.step(Input::Focus { game: None }, &scene(false))
}

#[test]
fn leaving_the_game_mutes_it() {
    let mut m = on();
    assert_eq!(backgrounded(&mut m), vec![Mute::On(10)]);
}

#[test]
fn returning_to_the_game_unmutes_it() {
    let mut m = on();
    backgrounded(&mut m);
    let back = m.step(Input::Focus { game: Some(GAME) }, &scene(true));
    assert_eq!(back, vec![Mute::Off(10)]);
}

#[test]
fn nothing_else_is_ever_touched() {
    let mut m = on();
    let actions = backgrounded(&mut m);
    assert!(actions.iter().all(|a| *a != Mute::On(20)), "muted the browser: {actions:?}");
}

/// Off by default means off: with the setting false nothing happens at all.
#[test]
fn disabled_does_nothing() {
    let mut m = Muter::new(false, BTreeSet::new());
    m.step(Input::Focus { game: Some(GAME) }, &scene(false));
    assert!(m.step(Input::Focus { game: None }, &scene(false)).is_empty());
}

/// Without a game ever focused there is nothing to mute — switching between
/// desktop windows must stay silent.
#[test]
fn no_game_seen_means_nothing_to_do() {
    let mut m = on();
    assert!(m.step(Input::Focus { game: None }, &scene(false)).is_empty());
}

/// A stream the user muted by hand is theirs. makima must not unmute it on the
/// way back, because it never muted it.
#[test]
fn a_stream_muted_by_the_user_stays_muted() {
    let mut m = on();
    m.step(Input::Focus { game: Some(GAME) }, &scene(true));
    assert!(m.step(Input::Focus { game: None }, &scene(true)).is_empty());
    assert!(m.step(Input::Focus { game: Some(GAME) }, &scene(true)).is_empty());
}

/// Switching between desktop windows repeatedly must not issue the same mute
/// over and over.
#[test]
fn muting_is_idempotent_while_away() {
    let mut m = on();
    backgrounded(&mut m);
    assert!(m.step(Input::Focus { game: None }, &scene(true)).is_empty());
}

/// A game in the background that opens another stream gets that one muted too.
#[test]
fn a_new_stream_from_the_background_game_is_muted() {
    let mut m = on();
    backgrounded(&mut m);
    let new = m.step(Input::StreamNew(stream(11, Some(GAME), "Hades", false)), &[]);
    assert_eq!(new, vec![Mute::On(11)]);
}

/// …but a new stream while the game is focused is left alone.
#[test]
fn a_new_stream_from_the_focused_game_is_left_alone() {
    let mut m = on();
    m.step(Input::Focus { game: Some(GAME) }, &scene(false));
    assert!(m.step(Input::StreamNew(stream(11, Some(GAME), "Hades", false)), &[]).is_empty());
}

/// The measured trap: WirePlumber stores mute per application name. A game that
/// exits while muted comes back muted; makima has to recognise that as its own
/// doing and release it.
#[test]
fn a_game_restored_muted_by_wireplumber_is_released() {
    let mut m = on();
    backgrounded(&mut m);
    assert!(m.ours().contains("Hades"));

    // The game exits while muted…
    m.step(Input::StreamGone { index: 10 }, &[]);
    // …and starts again later, focused, with WirePlumber restoring it muted.
    m.step(Input::Focus { game: Some(OTHER_GAME) }, &[]);
    let restored = stream(30, Some(OTHER_GAME), "Hades", true);
    assert_eq!(m.step(Input::StreamNew(restored), &[]), vec![Mute::Off(30)]);
    assert!(!m.ours().contains("Hades"), "still claims the name after releasing it");
}

/// The same trap survives a restart of makima, because the names are persisted
/// and handed back in through `Muter::new`.
#[test]
fn remembered_names_survive_a_restart() {
    let mut m = Muter::new(true, ["Hades".to_string()].into_iter().collect());
    m.step(Input::Focus { game: Some(GAME) }, &[]);
    let restored = stream(10, Some(GAME), "Hades", true);
    assert_eq!(m.step(Input::StreamNew(restored), &[]), vec![Mute::Off(10)]);
}

/// A muted stream under a name makima never touched is the user's — even when
/// the game is focused, makima does not unmute it.
#[test]
fn a_restored_mute_that_was_not_ours_is_left_alone() {
    let mut m = on();
    m.step(Input::Focus { game: Some(GAME) }, &[]);
    assert!(m.step(Input::StreamNew(stream(10, Some(GAME), "Hades", true)), &[]).is_empty());
}

/// Switching the setting off while a game is muted releases it immediately —
/// otherwise turning the feature off would leave a game silent.
#[test]
fn turning_it_off_releases_what_it_muted() {
    let mut m = on();
    backgrounded(&mut m);
    assert_eq!(m.step(Input::Enabled(false), &scene(true)), vec![Mute::Off(10)]);
}

/// Switching it on while a game is already in the background mutes it now, not
/// on the next window switch.
#[test]
fn turning_it_on_mutes_a_game_already_in_the_background() {
    let mut m = Muter::new(false, BTreeSet::new());
    m.step(Input::Focus { game: Some(GAME) }, &scene(false));
    m.step(Input::Focus { game: None }, &scene(false));
    assert_eq!(m.step(Input::Enabled(true), &scene(false)), vec![Mute::On(10)]);
}

/// The event lines `pactl subscribe` prints.
#[test]
fn subscribe_lines_are_parsed() {
    assert_eq!(parse_event("Event 'new' on sink-input #369"), Some(("new", 369)));
    assert_eq!(parse_event("Event 'remove' on sink-input #12"), Some(("remove", 12)));
    assert_eq!(parse_event("Event 'change' on sink #43"), None);
    assert_eq!(parse_event("garbage"), None);
}

/// The JSON shape `pactl --format=json list sink-inputs` produces, trimmed to
/// the fields used. A stream without an application name is skipped rather than
/// breaking the listing.
#[test]
fn stream_listing_is_parsed() {
    let json = br#"[
        {"index": 369, "mute": false,
         "properties": {"application.name": "Chromium", "application.process.id": "1"}},
        {"index": 43, "mute": true, "properties": {}}
    ]"#;
    let streams = parse_streams(json, |pid, app_id| resolve_game(pid, app_id, &HashMap::new()));
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].index, 369);
    assert_eq!(streams[0].app, "Chromium");
    assert!(!streams[0].muted);
    // PID 1 is init, never under a reaper.
    assert_eq!(streams[0].game, None);
}

/// The case that broke the first version: a Flatpak reports a PID from inside
/// its sandbox. Dolphin's real stream says `application.process.id = 13`. It
/// has to be matched by its portal app id, and the PID must not be used at all.
#[test]
fn a_sandboxed_stream_is_matched_by_its_app_id_not_its_pid() {
    let json = br#"[
        {"index": 510, "mute": false, "properties": {
            "application.name": "Dolphin Emulator",
            "application.process.id": "13",
            "pipewire.access.portal.app_id": "org.DolphinEmu.dolphin-emu"}}
    ]"#;
    let sandboxes: HashMap<String, u32> =
        [("org.DolphinEmu.dolphin-emu".to_string(), GAME)].into_iter().collect();

    let streams = parse_streams(json, |pid, app_id| resolve_game(pid, app_id, &sandboxes));
    assert_eq!(streams[0].game, Some(GAME));
}

/// A sandboxed app that is *not* running under a Steam game — Firefox as a
/// Flatpak, say — belongs to no game, whatever its PID happens to be.
#[test]
fn a_sandboxed_app_outside_any_game_belongs_to_none() {
    assert_eq!(resolve_game(Some(1), Some("org.mozilla.firefox"), &HashMap::new()), None);
}

/// The `.flatpak-info` format, as found in `/proc/<pid>/root/.flatpak-info`.
#[test]
fn flatpak_info_yields_the_app_id() {
    let info = "[Application]\nname=org.DolphinEmu.dolphin-emu\nruntime=runtime/org.kde.Platform\n\n[Instance]\nname=other\n";
    assert_eq!(parse_flatpak_info(info).as_deref(), Some("org.DolphinEmu.dolphin-emu"));
    // `name=` in a later section is not the app id.
    assert_eq!(parse_flatpak_info("[Instance]\nname=x\n"), None);
}

/// Off, a focus change needs no listing: that is what keeps a config that
/// never enables the feature from spawning `pactl` on every window switch.
#[test]
fn off_needs_no_streams_for_a_focus_change() {
    let m = Muter::new(false, BTreeSet::new());
    assert!(!m.needs_streams(&[Input::Focus { game: Some(GAME) }]));
    assert!(!m.needs_streams(&[Input::Focus { game: None }]));
}

/// Turning it on mutes a game already in the background, which needs the
/// streams; turning it off releases what was muted, which does too.
#[test]
fn switching_it_needs_streams() {
    let off = Muter::new(false, BTreeSet::new());
    assert!(off.needs_streams(&[Input::Enabled(true), Input::Focus { game: None }]));

    let mut m = on();
    backgrounded(&mut m);
    assert!(m.needs_streams(&[Input::Enabled(false)]));
}

/// Off, but something makima muted is still open: it must still be listed so
/// it can be released.
#[test]
fn off_with_a_stream_still_muted_needs_streams() {
    let mut m = on();
    backgrounded(&mut m);
    m.step(Input::Enabled(false), &[]); // the stream was not in the listing
    assert!(m.needs_streams(&[Input::Focus { game: None }]));
}
