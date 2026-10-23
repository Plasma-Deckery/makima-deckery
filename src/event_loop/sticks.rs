//! The four loops that turn a held stick or a held key into motion.
//!
//! Unlike everything else in this group they are not driven by events: they tick
//! on a timer and read the position the decoder last stored. Two analog, two
//! key-driven with an acceleration ramp. What each tick sends is decided by
//! `machine::plan_stick_tick` and `machine::plan_key_tick`.

use super::*;

impl EventReader {
    pub async fn cursor_loop(&self) {
        let (cursor, sensitivity, activation_modifiers) =
            if self.settings.lstick.function.as_str() == "cursor" {
                (
                    "left",
                    self.settings.lstick.sensitivity,
                    self.settings.lstick.activation_modifiers.clone(),
                )
            } else if self.settings.rstick.function.as_str() == "cursor" {
                (
                    "right",
                    self.settings.rstick.sensitivity,
                    self.settings.rstick.activation_modifiers.clone(),
                )
            } else {
                ("disabled", 0, vec![])
            };
        if sensitivity != 0 {
            while *self.device_is_connected.lock().await {
                {
                    let stick_position = if cursor == "left" {
                        self.lstick_position.lock().await
                    } else if cursor == "right" {
                        self.rstick_position.lock().await
                    } else {
                        break;
                    };
                    if stick_position[0] != 0 || stick_position[1] != 0 {
                        let modifiers = self.modifiers.lock().await;
                        if activation_modifiers.len() == 0 || activation_modifiers == *modifiers {
                            let active = crate::machine::stick_loop_active(*self.paused.lock().await, self.settings.cursor_when_paused, *self.gaming_mode.lock().await);
                            self.apply_frames(&crate::machine::plan_stick_tick(
                                crate::machine::CURSOR_AXES,
                                (stick_position[0], stick_position[1]),
                                self.settings.invert_cursor_axis,
                                &activation_modifiers, &modifiers, active,
                            )).await;
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(sensitivity)).await;
            }
        } else {
            return;
        }
    }

    pub async fn scroll_loop(&self) {
        let (scroll, sensitivity, activation_modifiers) =
            if self.settings.lstick.function.as_str() == "scroll" {
                (
                    "left",
                    self.settings.lstick.sensitivity,
                    self.settings.lstick.activation_modifiers.clone(),
                )
            } else if self.settings.rstick.function.as_str() == "scroll" {
                (
                    "right",
                    self.settings.rstick.sensitivity,
                    self.settings.rstick.activation_modifiers.clone(),
                )
            } else {
                ("disabled", 0, vec![])
            };
        if sensitivity != 0 {
            while *self.device_is_connected.lock().await {
                {
                    let stick_position = if scroll == "left" {
                        self.lstick_position.lock().await
                    } else if scroll == "right" {
                        self.rstick_position.lock().await
                    } else {
                        break;
                    };
                    if stick_position[0] != 0 || stick_position[1] != 0 {
                        let modifiers = self.modifiers.lock().await;
                        if activation_modifiers.len() == 0 || activation_modifiers == *modifiers {
                            let active = crate::machine::stick_loop_active(*self.paused.lock().await, self.settings.cursor_when_paused, *self.gaming_mode.lock().await);
                            self.apply_frames(&crate::machine::plan_stick_tick(
                                crate::machine::SCROLL_AXES,
                                (stick_position[0], stick_position[1]),
                                self.settings.invert_scroll_axis,
                                &activation_modifiers, &modifiers, active,
                            )).await;
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(sensitivity)).await;
            }
        } else {
            return;
        }
    }

    pub async fn key_cursor_loop(&self) {
        let (speed, acceleration, mut current_speed) = (
            if self.settings.cursor.speed == 0 {
                return;
            } else {
                self.settings.cursor.speed
            },
            if self.settings.cursor.acceleration.abs() > 1.0 {
                1.0
            } else {
                self.settings.cursor.acceleration.abs()
            },
            self.settings.cursor.speed as f32,
        );
        while *self.device_is_connected.lock().await {
            {
                let cursor_movement = self.cursor_movement.lock().await;
                let active = crate::machine::stick_loop_active(*self.paused.lock().await, self.settings.cursor_when_paused, *self.gaming_mode.lock().await);
                // The ramp is part of the decision, so the plan advances it and
                // hands back the new value — including the collapse to nothing
                // when no direction is held.
                let (frames, next_speed) = crate::machine::plan_key_tick(
                    crate::machine::CURSOR_AXES, *cursor_movement,
                    speed, acceleration, current_speed, active,
                );
                current_speed = next_speed;
                self.apply_frames(&frames).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    pub async fn key_scroll_loop(&self) {
        let (speed, acceleration, mut current_speed) = (
            if self.settings.scroll.speed == 0 {
                return;
            } else {
                self.settings.scroll.speed
            },
            if self.settings.scroll.acceleration.abs() > 1.0 {
                1.0
            } else {
                self.settings.scroll.acceleration.abs()
            },
            self.settings.scroll.speed as f32,
        );
        while *self.device_is_connected.lock().await {
            {
                let scroll_movement = self.scroll_movement.lock().await;
                let active = crate::machine::stick_loop_active(*self.paused.lock().await, self.settings.cursor_when_paused, *self.gaming_mode.lock().await);
                // The ramp is part of the decision, so the plan advances it and
                // hands back the new value — including the collapse to nothing
                // when no direction is held.
                let (frames, next_speed) = crate::machine::plan_key_tick(
                    crate::machine::SCROLL_AXES, *scroll_movement,
                    speed, acceleration, current_speed, active,
                );
                current_speed = next_speed;
                self.apply_frames(&frames).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}
