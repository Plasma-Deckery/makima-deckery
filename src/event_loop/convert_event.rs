//! What a logical event means, and making it happen.
//!
//! Between `read_event`, which says what arrived, and the ports, which carry
//! out what was decided. The deciding itself is in `machine` — the guard
//! ordering, the emit plans, the passthrough — so what is here is the applying:
//! take the plan, send its frames, apply its bookkeeping, hand the rest to a
//! port.

use super::*;

impl EventReader {
    /// Decide what one event means, and do it.
    ///
    /// The deciding is `machine::plan_route` — which branch applies and what
    /// changes before it — and the ordering of those guards is the part that
    /// used to carry a `TODO: structural debt` note. It was not that the order
    /// was wrong; it was that it was spread over eighty lines of early returns
    /// and could neither be read nor tested in one piece. It can now.
    pub(super) async fn convert_event(
        &self,
        default_event: InputEvent,
        event: Event,
        value: i32,
        send_zero: bool,
    ) {
        let gaming_mode = *self.gaming_mode.lock().await;

        // Both of these are decided without the config and have to be applied
        // before it is re-resolved below — a release has to clear what its press
        // emitted even if the binding has changed since.
        match crate::machine::held_update(event, value, gaming_mode) {
            crate::machine::HeldUpdate::Add => {
                let mut held = self.held_keys.lock().await;
                held.push(event);
                held.sort();
                held.dedup();
            }
            crate::machine::HeldUpdate::Remove => {
                self.held_keys.lock().await.retain(|&x| x != event);
            }
            crate::machine::HeldUpdate::Leave => {}
        }

        if crate::machine::releases_emitted(event, value) {
            let prev_keys = self.emitted_outputs.lock().await.remove(&event);
            if let Some(prev_keys) = prev_keys {
                let frames: Vec<_> =
                    prev_keys.iter().map(|key| crate::machine::key_up(*key)).collect();
                self.apply_frames(&frames).await;
            }
        }

        // A press may be the one that changed the focused window, so the config
        // is re-resolved before anything is decided against it.
        if value == 1 {
            self.update_config().await;
        }

        let paused = *self.paused.lock().await;
        let config = self.current_config.lock().await.clone();
        let mut modifiers = self.modifiers.lock().await.clone();
        modifiers.sort();
        modifiers.dedup();

        let route = {
            let trigger_ts = *self.gaming_mode_trigger_ts.lock().await;
            crate::machine::plan_route(&crate::machine::Input {
                event,
                value,
                config: &config,
                modifiers: &modifiers,
                gaming_mode,
                paused,
                trigger: self.gaming_mode_config.trigger.as_ref(),
                trigger_ts,
                now: std::time::Instant::now(),
            })
        };
        *self.gaming_mode_trigger_ts.lock().await = route.trigger_ts;

        match route.branch {
            crate::machine::Branch::ToggleGamingMode { enabled, preview } => {
                println!("deckery: Gaming Mode trigger — double-click (paused={paused})");
                self.outputs
                    .dispatch(&crate::machine::gaming_mode_toggle(enabled, preview))
                    .await;
                self.write_state().await;
                return;
            }
            crate::machine::Branch::ModifierIntercept { out_keys, emit, announce } => {
                if emit {
                    let frames: Vec<_> = out_keys
                        .iter()
                        .map(|key| crate::machine::key_at(*key, value))
                        .collect();
                    self.apply_frames(&frames).await;
                    if value == 1 {
                        self.store_emitted_outputs(&event, &out_keys).await;
                    }
                }
                if announce {
                    let hint = if value == 1 { self.hint_label(&event).await } else { None };
                    let announcement = crate::machine::keys_announcement(
                        &out_keys, value, false,
                        config.bindings.labels.get(&(event, vec![])).cloned(), hint, false,
                    );
                    if let Some(action) = announcement {
                        self.outputs.dispatch(&[action]).await;
                    }
                    self.write_state().await;
                }
                self.toggle_modifiers(event, value, &config).await;
                return;
            }
            crate::machine::Branch::Suppressed { track_modifier } => {
                if track_modifier {
                    self.toggle_modifiers(event, value, &config).await;
                }
                return;
            }
            crate::machine::Branch::Resolve => {}
        }

        match resolve_binding(&config.bindings, event, &modifiers, self.settings.chain_only) {
            ResolvedBinding::Keys { keys, label, no_pause, while_gaming: _, silent, is_combo, is_fallback } => {
                // release_keys: release held modifier output keys before emitting.
                //   true  → base binding with no modifiers held (normal press)
                //   false → combo or fallback; modifier keys must stay held
                // ignore_modifiers: don't re-emit modifier output keys after the action.
                //   true  → explicit combo (modifier was "consumed" by the combo)
                //   false → fallback or base binding (modifier remains independent)
                let release_keys    = !is_combo && !is_fallback;
                let ignore_modifiers = is_combo;
                let hint = if value == 1 && !is_combo {
                    self.hint_label(&event).await
                } else {
                    None
                };
                if let Some(action) = crate::machine::keys_announcement(
                    &keys, value, is_combo, label, hint, silent,
                ) {
                    self.outputs.dispatch(&[action]).await;
                }
                self.write_state().await;
                self.emit_event(&keys, value, &modifiers, &config, release_keys, ignore_modifiers, no_pause).await;
                if send_zero {
                    let mut modifiers = self.modifiers.lock().await.clone();
                    modifiers.sort();
                    modifiers.dedup();
                    self.emit_event(&keys, 0, &modifiers, &config, release_keys, ignore_modifiers, no_pause).await;
                }
                self.store_emitted_outputs(&event, &keys).await;
                return;
            }
            ResolvedBinding::Hold { keys } => {
                self.emit_event(&keys, value, &modifiers, &config, false, false, false).await;
                return;
            }
            ResolvedBinding::Command { commands, label, no_pause, while_gaming: _, silent, .. } => {
                if value == 1 {
                    let mut actions = vec![crate::machine::command_announcement(
                        &commands, label.clone(), silent,
                    )];
                    if !paused || no_pause {
                        // The label travels with it so a failure can name the
                        // binding the user pressed rather than the command line.
                        actions.push(crate::output::action::OutputAction::Command {
                            argv: commands.clone(),
                            label,
                        });
                        // Running a command counts as having used the modifier
                        // that was held, so releasing it does not also tap.
                        // Only when it actually runs, which is why this sits
                        // inside the same condition.
                        *self.modifier_was_activated.lock().await = true;
                    }
                    self.outputs.dispatch(&actions).await;
                }
                self.write_state().await;
                return;
            }
            ResolvedBinding::Movement { movement, .. } => {
                if value <= 1 && !paused {
                    self.emit_movement(&movement, value).await;
                }
                return;
            }
            ResolvedBinding::Unbound => {}
        }
        self.emit_nonmapped_event(default_event, event, value, &modifiers, &config)
            .await;
    }

    /// Send a plan's frames to the output devices.
    ///
    /// One lock for the whole plan, so every frame of one decision is contiguous
    /// as the kernel reads it. `Frame` is synthesised and stamped here, which is
    /// what the old path did; `Forward` keeps the stamp it arrived with, which is
    /// why it is a separate kind.
    ///
    /// This does change one thing: `key_cursor_loop` used to take the lock once
    /// per axis, so another producer could split a pointer motion between X and
    /// Y. It cannot any more. That is deliberate — the other three loops already
    /// behaved this way, and a motion split in half was never the intent.
    pub(super) async fn apply_frames(&self, actions: &[crate::output::action::OutputAction]) {
        if actions.is_empty() {
            return;
        }
        self.outputs.dispatch(actions).await;
    }

    /// Emit what a resolved key binding amounts to.
    ///
    /// The decision is `machine::plan_emit`, which is pure and tested; this
    /// applies it. The two used to be one interleaved loop — read state, mutate
    /// state, emit, repeat — and that interleaving was what made the binding
    /// engine impossible to test without a kernel.
    ///
    /// The frames go out first and under one lock, so every frame of one
    /// decision reaches the kernel contiguously. The modifier bookkeeping
    /// follows, which is the one observable change from the old order: it used
    /// to happen between frames, and each of those calls publishes state at its
    /// end, so those publishes now land after the frames rather than among
    /// them. Only the overlay sees that, and only as timing.
    pub(super) async fn emit_event(
        &self,
        event_list: &Vec<Key>,
        value: i32,
        modifiers: &Vec<Event>,
        config: &Config,
        release_keys: bool,
        ignore_modifiers: bool,
        no_pause: bool,
    ) {
        let paused = *self.paused.lock().await;

        let plan = {
            let mut modifier_was_activated = self.modifier_was_activated.lock().await;
            let plan = crate::machine::plan_emit(&crate::machine::EmitRequest {
                config,
                event_list,
                value,
                modifiers,
                release_keys,
                ignore_modifiers,
                modifier_was_activated: *modifier_was_activated,
                paused,
                no_pause,
            });
            *modifier_was_activated = plan.modifier_was_activated;
            plan
        };

        self.apply_frames(&plan.frames).await;

        for (modifier, value) in plan.toggles {
            self.toggle_modifiers(modifier, value, config).await;
        }
    }

    pub(super) async fn emit_nonmapped_event(
        &self,
        default_event: InputEvent,
        event: Event,
        value: i32,
        modifiers: &Vec<Event>,
        config: &Config,
    ) {
        if *self.paused.lock().await {
            // Check whether this exact (trigger, current-modifiers) combo has no_pause = true.
            // If so, fall through to normal processing so the binding executes even when paused.
            let current_mods = self.modifiers.lock().await.clone();
            let is_no_pause = config.bindings.no_pause.contains(&(event, current_mods.clone()))
                || config.bindings.no_pause.contains(&(event, vec![]));
            if !is_no_pause {
                // When paused, still track modifier buttons (BTN_TL, BTN_MODE, etc.)
                // so the HUD can display combo overlays while open.
                // For non-modifier buttons, just write state to reflect active_buttons.
                if config.mapped_modifiers.all.contains(&event) {
                    self.toggle_modifiers(event, value, config).await;
                } else {
                    self.write_state().await;
                }
                return;
            }
        }
        // Passthrough keys are held state — last_action is only for combos and commands.
        let plan = {
            let mut modifier_was_activated = self.modifier_was_activated.lock().await;
            let plan = crate::machine::plan_nonmapped(
                config, event, &default_event, value, modifiers, *modifier_was_activated,
            );
            *modifier_was_activated = plan.modifier_was_activated;
            plan
        };
        self.apply_frames(&plan.frames).await;
        for (modifier, value) in plan.toggles {
            self.toggle_modifiers(modifier, value, config).await;
        }
    }

    /// Pass an event the reader has no binding for straight through.
    pub(super) async fn emit_default_event(&self, event: InputEvent) {
        let paused = *self.paused.lock().await;
        self.apply_frames(&crate::machine::plan_default(&event, paused)).await;
    }

    pub(super) async fn emit_movement(&self, movement: &Relative, value: i32) {
        let mut cursor_movement = self.cursor_movement.lock().await;
        let mut scroll_movement = self.scroll_movement.lock().await;
        match movement {
            Relative::Cursor(Cursor::CURSOR_UP) => cursor_movement.1 = -value,
            Relative::Cursor(Cursor::CURSOR_DOWN) => cursor_movement.1 = value,
            Relative::Cursor(Cursor::CURSOR_LEFT) => cursor_movement.0 = -value,
            Relative::Cursor(Cursor::CURSOR_RIGHT) => cursor_movement.0 = value,
            Relative::Scroll(Scroll::SCROLL_UP) => scroll_movement.1 = -value,
            Relative::Scroll(Scroll::SCROLL_DOWN) => scroll_movement.1 = value,
            Relative::Scroll(Scroll::SCROLL_LEFT) => scroll_movement.0 = -value,
            Relative::Scroll(Scroll::SCROLL_RIGHT) => scroll_movement.0 = value,
        };
    }

    pub(super) async fn toggle_modifiers(&self, modifier: Event, value: i32, config: &Config) {
        {
            let mut modifiers = self.modifiers.lock().await;
            if config.mapped_modifiers.all.contains(&modifier) {
                match value {
                    1 => {
                        modifiers.push(modifier);
                        modifiers.sort();
                        modifiers.dedup();
                    }
                    0 => modifiers.retain(|&x| x != modifier),
                    _ => {}
                }
            }
        } // lock released here
        self.write_state().await;
    }
}
