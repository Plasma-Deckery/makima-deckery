//! Reading the hardware: raw evdev in, logical events out.
//!
//! One match over `(EventType, RelativeAxisType, AbsoluteAxisType, is_tablet)`
//! that turns what the kernel reports into what the rest of Deckery talks about
//! — a button, a direction, a trigger edge — and hands each one to
//! `convert_event`. Nothing here decides what an event *means*; it decides what
//! an event *is*.
//!
//! It is the largest thing in this group and it used to be nearly twice as
//! large: the two sticks were written out separately, the two triggers as well,
//! with a third copy of the stick logic inside the Stadia branch. What is left
//! is the decoding that genuinely differs per axis kind.

use super::*;

impl EventReader {
    pub async fn read_events(&self) {
        println!("deckery: event_loop: started.");
        let (
            mut dpad_values,
            mut lstick_values,
            mut rstick_values,
            mut triggers_values,
            mut abs_wheel_position,
        ) = ((0, 0), (0, 0), (0, 0), (0, 0), 0);
        let mut rx = self.event_rx.lock().await;
        let mut pen_events: Vec<InputEvent> = Vec::new();
        // is_tablet and max_abs_wheel were queried once at device open time
        // (SteamDeckController::start_event_task) and don't change across reconnects.
        let is_tablet = self.is_tablet;
        let max_abs_wheel = self.max_abs_wheel;
        // Dirty flags for stick axes — set whenever ABS_X/Y or ABS_RX/RY arrive.
        let mut lstick_dirty = false;
        let mut rstick_dirty = false;
        // Dirty flag for IMU axes (ABS_HAT2X/Y).
        let mut imu_dirty = false;
        // Rate-limit state.json writes from stick/gyro movement to ~60 Hz.
        // (Trackpad state.json writes are handled by trackpad_router, driven by hidraw frames.)
        let mut last_stick_state_write = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_millis(17))
            .unwrap_or(std::time::Instant::now());
        loop {
            let event = match rx.recv().await {
                Some(ControllerEvent::Input(e)) => e,
                Some(ControllerEvent::Reconnected) => {
                    // Device briefly disappeared (suspend/resume). Release all held
                    // output keys so the compositor doesn't see stuck modifiers.
                    self.release_all_held().await;
                    continue;
                }
                Some(ControllerEvent::ReleaseAll) => {
                    // Another process is about to grab the device exclusively.
                    // Release all held output keys immediately to avoid stuck keys
                    // while the grab is active.
                    self.release_all_held().await;
                    continue;
                }
                None => {
                    // Channel closed: the reconnecting task gave up (device genuinely
                    // gone, device_error_notify already fired by that task).
                    break;
                }
            };
            match (
                event.event_type(),
                RelativeAxisType(event.code()),
                AbsoluteAxisType(event.code()),
                is_tablet,
            ) {
                (EventType::KEY, _, _, _) => match Key(event.code()) {
                    Key::BTN_TOOL_PEN
                    | Key::BTN_TOOL_RUBBER
                    | Key::BTN_TOOL_BRUSH
                    | Key::BTN_TOOL_PENCIL
                    | Key::BTN_TOOL_AIRBRUSH
                    | Key::BTN_TOOL_MOUSE
                    | Key::BTN_TOOL_LENS
                        if is_tablet =>
                    {
                        pen_events.push(event)
                    }
                    // Everything the Deck sends lands here. Whether a key is
                    // passed on — a trackpad's own click is swallowed, because
                    // trackpad_router reads it from hidraw — is decided in
                    // `decode::key_event`.
                    key => {
                        let decoded = crate::decode::key_event(
                            key, self.trackpad.left.has_handler(), self.trackpad.right.has_handler(),
                        );
                        if let Some(logical) = decoded {
                            self.convert_event(event, logical, event.value(), false).await
                        }
                    }
                },
                (
                    EventType::RELATIVE,
                    RelativeAxisType::REL_WHEEL | RelativeAxisType::REL_WHEEL_HI_RES,
                    _,
                    _,
                ) => match event.value() {
                    -120 => {
                        self.convert_event(event, Event::Axis(Axis::SCROLL_WHEEL_DOWN), 1, true)
                            .await;
                    }
                    120 => {
                        self.convert_event(event, Event::Axis(Axis::SCROLL_WHEEL_UP), 1, true)
                            .await;
                    }
                    _ => {}
                },
                (EventType::ABSOLUTE, _, AbsoluteAxisType::ABS_WHEEL, _) => {
                    let value = event.value();
                    if value != 0 && abs_wheel_position != 0 {
                        let gap = value - abs_wheel_position;
                        if gap < -max_abs_wheel / 2 {
                            self.convert_event(event, Event::Axis(Axis::ABS_WHEEL_CW), 1, true)
                                .await;
                        } else if gap > max_abs_wheel / 2 {
                            self.convert_event(event, Event::Axis(Axis::ABS_WHEEL_CCW), 1, true)
                                .await;
                        } else if value > abs_wheel_position {
                            self.convert_event(event, Event::Axis(Axis::ABS_WHEEL_CW), 1, true)
                                .await;
                        } else if value < abs_wheel_position {
                            self.convert_event(event, Event::Axis(Axis::ABS_WHEEL_CCW), 1, true)
                                .await;
                        }
                    }
                    abs_wheel_position = value;
                }
                (EventType::ABSOLUTE, _, AbsoluteAxisType::ABS_MISC, _) => {
                    if is_tablet == false && event.value() == 0 {
                        abs_wheel_position = 0
                    } else {
                        self.emit_default_event(event).await;
                    }
                }
                (EventType::ABSOLUTE, _, _, true) => pen_events.push(event),
                (_, _, AbsoluteAxisType::ABS_HAT0X | AbsoluteAxisType::ABS_HAT0Y, _) => {
                    // On a Deck these are the left pad's position, which
                    // trackpad_router reads from hidraw when a handler owns the
                    // pad and which is dropped when the pad is disabled — never
                    // a hat: the Deck's D-pad arrives as key edges. Only an
                    // evdev-class controller with a real hat switch decodes it.
                    if !self.is_deck {
                        let is_x = event.code() == AbsoluteAxisType::ABS_HAT0X.0;
                        let held = if is_x { dpad_values.0 } else { dpad_values.1 };
                        let (events, next) = crate::decode::decode_hat(is_x, event.value(), held);
                        if is_x { dpad_values.0 = next } else { dpad_values.1 = next }
                        for (axis, v) in events {
                            self.convert_event(event, Event::Axis(axis), v, false).await;
                        }
                    }
                }
                // RPAD position (ABS_HAT1X/Y) is read directly from hidraw by trackpad_router
                // instead — swallow these here so they don't fall through to
                // emit_default_event and leak onto the generic ABS device.
                (_, _, AbsoluteAxisType::ABS_HAT1X | AbsoluteAxisType::ABS_HAT1Y, _) => {}
                // IMU (gyroscope/accelerometer): ABS_HAT2X/Y, range 0..32767.
                // Stored for state export; not forwarded.
                (_, _, AbsoluteAxisType::ABS_HAT2X, _) => {
                    self.imu_raw.lock().await.0 = event.value() as i32;
                    imu_dirty = true;
                }
                (_, _, AbsoluteAxisType::ABS_HAT2Y, _) => {
                    self.imu_raw.lock().await.1 = event.value() as i32;
                    imu_dirty = true;
                }
                (
                    EventType::ABSOLUTE,
                    _,
                    AbsoluteAxisType::ABS_X
                    | AbsoluteAxisType::ABS_Y
                    | AbsoluteAxisType::ABS_RX
                    | AbsoluteAxisType::ABS_RY,
                    false,
                ) => {
                    // One arm for both sticks. It used to be two of 140 lines
                    // each that differed only in the axis offset, the `Axis`
                    // variants they named, and whether a guard sat in the
                    // pattern or inside the arm — copy-paste twins with no
                    // behavioural difference between them.
                    let (stick, is_x) = crate::decode::stick_for_code(event.code());
                    let (settings, position, raw_cell, values) =
                        if stick == crate::machine::Stick::Left {
                            (&self.settings.lstick, &self.lstick_position,
                             &self.lstick_raw, &mut lstick_values)
                        } else {
                            (&self.settings.rstick, &self.rstick_position,
                             &self.rstick_raw, &mut rstick_values)
                        };

                    // The raw value is recorded whatever the stick is for, so the
                    // overlay can draw it even when nothing is bound.
                    {
                        let raw = event.value() as i32;
                        let mut cell = raw_cell.lock().await;
                        if is_x { cell.0 = raw; } else { cell.1 = raw; }
                        if stick == crate::machine::Stick::Left {
                            lstick_dirty = true;
                        } else {
                            rstick_dirty = true;
                        }
                    }

                    match crate::decode::stick_update(
                        &settings.function, stick, is_x, event.value() as i32,
                        settings.deadzone, self.settings.axis_16_bit, *values,
                    ) {
                        crate::decode::StickUpdate::Position { index, value } => {
                            position.lock().await[index] = value;
                        }
                        crate::decode::StickUpdate::Bind { events, held } => {
                            *values = held;
                            for (axis, v) in events {
                                self.convert_event(event, Event::Axis(axis), v, false).await;
                            }
                        }
                        crate::decode::StickUpdate::Ignore => {}
                    }
                }

                (
                    EventType::ABSOLUTE,
                    _,
                    AbsoluteAxisType::ABS_Z | AbsoluteAxisType::ABS_RZ,
                    false,
                ) => {
                    // One arm for both triggers, and the same for the Stadia
                    // case, where these two axes *are* the right stick. That
                    // second half used to be a third copy of the stick bind
                    // logic, after the two in the stick arms.
                    let is_left = event.code() == AbsoluteAxisType::ABS_Z.0;
                    if !self.settings.stadia {
                        // A trigger is a button with a travel: it fires once on
                        // the way in and once on the way back to rest.
                        let held = if is_left { triggers_values.0 } else { triggers_values.1 };
                        let axis = if is_left { Axis::ABS_Z } else { Axis::ABS_RZ };
                        let next = match (event.value(), held) {
                            (0, 1) => {
                                self.convert_event(event, Event::Axis(axis), 0, false).await;
                                Some(0)
                            }
                            (_, 0) => {
                                self.convert_event(event, Event::Axis(axis), 1, false).await;
                                Some(1)
                            }
                            _ => None,
                        };
                        if let Some(next) = next {
                            if is_left { triggers_values.0 = next } else { triggers_values.1 = next }
                        }
                    } else {
                        // On a Stadia controller these are the right stick's
                        // axes, so the same decision applies as for a stick.
                        let value = crate::machine::axis_value(
                            event.value() as i32,
                            self.settings.rstick.deadzone,
                            self.settings.axis_16_bit,
                        );
                        match self.settings.rstick.function.as_str() {
                            "cursor" | "scroll" => {
                                self.rstick_position.lock().await[if is_left { 0 } else { 1 }]
                                    = value;
                            }
                            "bind" => {
                                let (events, next) = crate::machine::decode_stick_bind(
                                    crate::machine::Stick::Right,
                                    is_left,
                                    value.signum(),
                                    rstick_values,
                                );
                                rstick_values = next;
                                for (axis, v) in events {
                                    self.convert_event(event, Event::Axis(axis), v, false).await;
                                }
                            }
                            _ => {}
                        }
                    }
                }

                (EventType::MISC, _, _, true) => {
                    if evdev::MiscType(event.code()) == evdev::MiscType::MSC_SERIAL {
                        pen_events.push(event);
                        // One frame carrying the whole accumulated report, with
                        // the pen's own timestamps — which is why this is a
                        // Forward and not a Frame.
                        self.apply_frames(&[crate::output::action::OutputAction::Forward {
                            device: crate::output::OutputDevice::Abs,
                            events: pen_events.iter().map(|e| crate::output::action::Raw(*e)).collect(),
                        }]).await;
                        pen_events.clear()
                    }
                }
                (EventType::SYNCHRONIZATION, _, _, _) => {
                    // Trackpad handling (position, touch, gestures, its own state.json
                    // writes) has moved entirely to trackpad_router, driven by hidraw
                    // frames. This SYN handler now only rate-limits state.json writes
                    // triggered by stick/gyro movement.
                    let stick_changed = lstick_dirty || rstick_dirty || imu_dirty;
                    if lstick_dirty { lstick_dirty = false; }
                    if rstick_dirty { rstick_dirty = false; }
                    if imu_dirty { imu_dirty = false; }
                    if crate::decode::analog_write_due(
                        stick_changed, last_stick_state_write.elapsed(),
                    ) {
                        last_stick_state_write = std::time::Instant::now();
                        self.write_state_analog().await;
                    }
                }
                _ => self.emit_default_event(event).await,
            }
        }
        // device_error_notify is fired by the reconnecting reader task (in
        // steam_deck_controller) when it gives up — no need to fire it here.
        let mut device_is_connected = self.device_is_connected.lock().await;
        *device_is_connected = false;

        println!(
            "Disconnected device \"{}\".\n",
            self.current_config.lock().await.name
        );
    }
}
