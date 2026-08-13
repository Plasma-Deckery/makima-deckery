//! Trackpad device lifecycle: setup and handler orchestration.
//!
//! `EventReader` owns the evdev event loop and all input-remapping state.
//! This module owns everything related to the *trackpad* side of a device
//! session: writing KDE libinput defaults, creating virtual MT devices,
//! connecting the hidraw channel, parsing handler configs, and running the
//! router + per-handler async tasks for the lifetime of the session.
//!
//! Separation rationale: `event_reader.rs` is the tight inner loop that must
//! stay focused on reading evdev events and resolving remaps. Trackpad setup
//! touches four different subsystems (kcminputrc, uinput, hidraw, channels)
//! and spawns multiple long-running tasks — none of that belongs in an event
//! reader. `TrackpadSession::setup` wires it all up; `TrackpadSession::run`
//! drives it until the session ends.

use crate::config::TrackpadConfig;
use crate::gesture_pad::{self, GesturePadConfig};
use crate::kde_input_defaults::{self, GestureKdeConfig, PadKdeConfig};
use crate::mt_trackpad::{self, HapticChain, MovementHaptic, MtTrackpadConfig};
use crate::pad_hidraw::{self, HapticCommand, HapticPad, PadFrame};
use crate::trackpad::PadState;
use crate::trackpad_router::{self, GestureEvent, SinglePadFrame, StateWrite};
use crate::virtual_devices::VirtualDevices;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

pub struct TrackpadSession {
    pad_rx:      Option<mpsc::Receiver<PadFrame>>,
    haptic_tx:   Option<mpsc::Sender<HapticCommand>>,
    left_tx:     Option<mpsc::Sender<SinglePadFrame>>,
    left_rx:     Option<mpsc::Receiver<SinglePadFrame>>,
    right_tx:    Option<mpsc::Sender<SinglePadFrame>>,
    right_rx:    Option<mpsc::Receiver<SinglePadFrame>>,
    combined_tx: Option<mpsc::Sender<GestureEvent>>,
    combined_rx: Option<mpsc::Receiver<GestureEvent>>,

    left_press_chain:    HapticChain,
    left_release_chain:  Option<HapticChain>,
    left_movement_pulse: Option<MovementHaptic>,

    right_press_chain:    HapticChain,
    right_release_chain:  Option<HapticChain>,
    right_movement_pulse: Option<MovementHaptic>,

    gesture_pad_config: GesturePadConfig,
}

impl TrackpadSession {
    /// Wires up the full trackpad stack for one device session:
    /// writes KDE libinput defaults, creates virtual MT uinput nodes,
    /// connects hidraw, parses handler configs, and builds the routing
    /// channels. Call this before `run` and before anything reads the pad.
    pub async fn setup(
        trackpad: &TrackpadConfig,
        virt_dev: &Arc<Mutex<VirtualDevices>>,
        device_path: &std::path::Path,
    ) -> Self {
        // Validate modes early so the warning appears before any device work.
        for (side, mode) in [("left", &trackpad.left.mode), ("right", &trackpad.right.mode)] {
            if !["disabled", "mt-trackpad", "scroll"].contains(&mode.as_str()) {
                eprintln!(
                    "[makima] Warning: unrecognised trackpad mode {:?} for {} pad, treating as disabled.",
                    mode, side
                );
            }
        }

        // Write kcminputrc before the uinput nodes appear so KWin picks up the
        // settings on first device discovery (see kde_input_defaults.rs).
        kde_input_defaults::ensure_kde_input_defaults(
            &PadKdeConfig::from_toml_value(&trackpad.left.kde_config),
            &PadKdeConfig::from_toml_value(&trackpad.right.kde_config),
            &GestureKdeConfig::from_toml_value(&trackpad.gesture_kde_config),
        );

        {
            let mut vd = virt_dev.lock().await;
            vd.enable_trackpads(
                trackpad.left.mode == "mt-trackpad",
                trackpad.right.mode == "mt-trackpad",
            );
            if trackpad.combined_gesture_device {
                vd.enable_gesture_pad();
            }
        }

        let (pad_rx, haptic_tx) = match pad_hidraw::spawn(device_path) {
            Some((rx, tx)) => {
                (Some(rx), Some(tx))
            }
            None => {
                println!(
                    "makima: no hidraw sibling found for {:?}, trackpad position/touch will not be available",
                    device_path
                );
                (None, None)
            }
        };

        // Each handler self-parses its own config slice — Core only knows
        // mode and click_pressure; everything else is handler-owned.
        let left_mt  = MtTrackpadConfig::from_toml_value(&trackpad.left.handler_config);
        let right_mt = MtTrackpadConfig::from_toml_value(&trackpad.right.handler_config);
        let gesture_pad_config = GesturePadConfig::from_toml_value(&trackpad.gesture_handler_config);

        let (left_tx, left_rx) = if trackpad.left.mode == "mt-trackpad" {
            let (tx, rx) = mpsc::channel(64);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let (right_tx, right_rx) = if trackpad.right.mode == "mt-trackpad" {
            let (tx, rx) = mpsc::channel(64);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let (combined_tx, combined_rx) = if trackpad.combined_gesture_device {
            let (tx, rx) = mpsc::channel(64);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        Self {
            pad_rx,
            haptic_tx,
            left_tx,
            left_rx,
            right_tx,
            right_rx,
            combined_tx,
            combined_rx,
            left_press_chain:    left_mt.press_chain(),
            left_release_chain:  left_mt.release_chain(),
            left_movement_pulse: left_mt.movement_pulse(),
            right_press_chain:    right_mt.press_chain(),
            right_release_chain:  right_mt.release_chain(),
            right_movement_pulse: right_mt.movement_pulse(),
            gesture_pad_config,
        }
    }

    /// Returns a clone of the haptic command sender, if one was established.
    /// `None` when no hidraw sibling was found (trackpad position not available).
    /// Used by `EventReader` to fire haptics on Gaming Mode toggle.
    pub fn haptic_tx(&self) -> Option<mpsc::Sender<HapticCommand>> {
        self.haptic_tx.clone()
    }

    /// Runs the trackpad router and all per-handler tasks until the session
    /// ends (hidraw channel closes). Consumes `self`.
    pub async fn run(
        self,
        virt_dev: &Arc<Mutex<VirtualDevices>>,
        lpad: &PadState,
        rpad: &PadState,
        gesture_session: &Arc<Mutex<bool>>,
        state_tx: mpsc::Sender<StateWrite>,
        gaming_mode: Arc<Mutex<bool>>,
    ) {
        let left_haptic    = self.haptic_tx.clone();
        let right_haptic   = self.haptic_tx.clone();
        let gesture_haptic = self.haptic_tx;

        tokio::join!(
            async {
                if let Some(rx) = self.pad_rx {
                    trackpad_router::run(
                        rx, lpad, rpad, gesture_session,
                        state_tx, self.left_tx, self.right_tx, self.combined_tx,
                        gaming_mode,
                    )
                    .await;
                }
            },
            async {
                if let Some(rx) = self.left_rx {
                    mt_trackpad::run_single(
                        rx, virt_dev, lpad,
                        left_haptic, HapticPad::Left,
                        self.left_press_chain,
                        self.left_release_chain,
                        self.left_movement_pulse,
                    )
                    .await;
                }
            },
            async {
                if let Some(rx) = self.right_rx {
                    mt_trackpad::run_single(
                        rx, virt_dev, rpad,
                        right_haptic, HapticPad::Right,
                        self.right_press_chain,
                        self.right_release_chain,
                        self.right_movement_pulse,
                    )
                    .await;
                }
            },
            async {
                if let Some(rx) = self.combined_rx {
                    gesture_pad::run(rx, virt_dev, gesture_haptic, self.gesture_pad_config).await;
                }
            },
        );
    }
}
