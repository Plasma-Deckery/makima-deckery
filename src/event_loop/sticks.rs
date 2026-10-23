//! The four loops that turn a held stick or a held key into motion.
//!
//! Unlike everything else in this group they are not driven by events: they tick
//! on a timer and read the position the decoder last stored. Two analog, two
//! key-driven with an acceleration ramp. What each tick sends is decided by
//! `machine::plan_stick_tick` and `machine::plan_key_tick`.

use super::*;

impl EventReader {
    /// One tick loop for a stick bound to `"cursor"` or to `"scroll"`.
    ///
    /// The two used to be written out separately and were identical once the
    /// word `cursor` was swapped for `scroll` — forty-six lines twice over.
    /// What genuinely differs is the function name a stick has to claim, which
    /// axes the motion goes to, and which invert setting applies, so those are
    /// the three arguments.
    async fn analog_loop(
        &self,
        function: &str,
        axes: (u16, u16),
        invert: bool,
    ) {
        // Whichever stick claims this function; neither, and the loop never
        // starts. A sensitivity of zero means the same thing, which is how a
        // stick is switched off without being unbound.
        let (side, sensitivity, activation_modifiers) =
            if self.settings.lstick.function == function {
                (Side::Left, self.settings.lstick.sensitivity,
                 self.settings.lstick.activation_modifiers.clone())
            } else if self.settings.rstick.function == function {
                (Side::Right, self.settings.rstick.sensitivity,
                 self.settings.rstick.activation_modifiers.clone())
            } else {
                return;
            };
        if sensitivity == 0 {
            return;
        }

        while *self.device_is_connected.lock().await {
            {
                let position = match side {
                    Side::Left => self.lstick_position.lock().await,
                    Side::Right => self.rstick_position.lock().await,
                };
                if position[0] != 0 || position[1] != 0 {
                    let modifiers = self.modifiers.lock().await;
                    let active = crate::machine::stick_loop_active(
                        *self.paused.lock().await,
                        self.settings.cursor_when_paused,
                        *self.gaming_mode.lock().await,
                    );
                    self.apply_frames(&crate::machine::plan_stick_tick(
                        axes,
                        (position[0], position[1]),
                        invert,
                        &activation_modifiers,
                        &modifiers,
                        active,
                    )).await;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(sensitivity)).await;
        }
    }

    pub async fn cursor_loop(&self) {
        self.analog_loop("cursor", crate::machine::CURSOR_AXES,
                         self.settings.invert_cursor_axis).await;
    }

    pub async fn scroll_loop(&self) {
        self.analog_loop("scroll", crate::machine::SCROLL_AXES,
                         self.settings.invert_scroll_axis).await;
    }

    /// One tick loop for a key-driven cursor or scroll ramp.
    ///
    /// Also two identical copies before, differing only in the word. The ramp
    /// itself is `machine::plan_key_tick`; what is here is the clamping of the
    /// configured acceleration and the ten-millisecond beat.
    async fn key_loop(
        &self,
        axes: (u16, u16),
        movement: &Mutex<(i32, i32)>,
        config: &crate::settings::Movement,
    ) {
        if config.speed == 0 {
            return;
        }
        let speed = config.speed;
        // A configured acceleration above 1 would ramp faster than the speed it
        // is ramping towards, so it is clamped rather than rejected.
        let acceleration = config.acceleration.abs().min(1.0);
        let mut current_speed = speed as f32;

        while *self.device_is_connected.lock().await {
            {
                let movement = movement.lock().await;
                let active = crate::machine::stick_loop_active(
                    *self.paused.lock().await,
                    self.settings.cursor_when_paused,
                    *self.gaming_mode.lock().await,
                );
                // The ramp is part of the decision, so the plan advances it and
                // hands back the new value — including the collapse to nothing
                // when no direction is held.
                let (frames, next_speed) = crate::machine::plan_key_tick(
                    axes, *movement, speed, acceleration, current_speed, active,
                );
                current_speed = next_speed;
                self.apply_frames(&frames).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    pub async fn key_cursor_loop(&self) {
        self.key_loop(crate::machine::CURSOR_AXES, &self.cursor_movement,
                      &self.settings.cursor).await;
    }

    pub async fn key_scroll_loop(&self) {
        self.key_loop(crate::machine::SCROLL_AXES, &self.scroll_movement,
                      &self.settings.scroll).await;
    }
}

/// Which stick a loop ended up claiming.
#[derive(Clone, Copy)]
enum Side {
    Left,
    Right,
}
