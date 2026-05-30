use crate::active_client::*;
use crate::config::{parse_modifiers, Associations, Axis, Cursor, Event, Relative, Scroll};
use crate::state_export::LastAction;
use crate::udev_monitor::{Client, Environment, Server};
use crate::virtual_devices::VirtualDevices;
use crate::Config;
use evdev::{AbsoluteAxisType, EventStream, EventType, InputEvent, Key, RelativeAxisType};
use fork::{fork, setsid, Fork};
use std::{
    future::Future,
    io::{BufRead, BufReader},
    option::Option,
    pin::Pin,
    process::{Command, Stdio},
    str::FromStr,
    sync::Arc,
};
use tokio::sync::{Mutex, Notify};
use tokio::net::UnixListener;
use tokio_stream::StreamExt;

struct Stick {
    function: String,
    sensitivity: u64,
    deadzone: i32,
    activation_modifiers: Vec<Event>,
}

struct Movement {
    speed: i32,
    acceleration: f32,
}

struct Settings {
    lstick: Stick,
    rstick: Stick,
    invert_cursor_axis: bool,
    invert_scroll_axis: bool,
    axis_16_bit: bool,
    stadia: bool,
    cursor: Movement,
    scroll: Movement,
    chain_only: bool,
    layout_switcher: Option<(Event, Vec<Event>)>,
    notify_layout_switch: bool,
    /// If true, cursor/scroll loops keep running even while makima is paused.
    /// Default: false. Set CURSOR_WHEN_PAUSED = "true" in [settings].
    cursor_when_paused: bool,
}

pub struct EventReader {
    config: Vec<Config>,
    stream: Arc<Mutex<EventStream>>,
    virt_dev: Arc<Mutex<VirtualDevices>>,
    lstick_position: Arc<Mutex<Vec<i32>>>,
    rstick_position: Arc<Mutex<Vec<i32>>>,
    cursor_movement: Arc<Mutex<(i32, i32)>>,
    scroll_movement: Arc<Mutex<(i32, i32)>>,
    modifiers: Arc<Mutex<Vec<Event>>>,
    modifier_was_activated: Arc<Mutex<bool>>,
    paused: Arc<Mutex<bool>>,
    last_action: Arc<Mutex<Option<LastAction>>>,
    held_keys: Arc<Mutex<Vec<Event>>>,
    device_is_connected: Arc<Mutex<bool>>,
    device_error_notify: Arc<Notify>,
    active_layout: Arc<Mutex<u16>>,
    current_config: Arc<Mutex<Config>>,
    environment: Environment,
    settings: Settings,
    active_client: Arc<Mutex<Client>>,
    window_changed: Arc<Notify>,
}

impl EventReader {
    pub fn new(
        config: Vec<Config>,
        virt_dev: Arc<Mutex<VirtualDevices>>,
        stream: Arc<Mutex<EventStream>>,
        modifiers: Arc<Mutex<Vec<Event>>>,
        modifier_was_activated: Arc<Mutex<bool>>,
        environment: Environment,
        device_error_notify: Arc<Notify>,
        active_client: Arc<Mutex<Client>>,
        window_changed: Arc<Notify>,
    ) -> Self {
        let mut position_vector: Vec<i32> = Vec::new();
        for i in [0, 0] {
            position_vector.push(i)
        }
        let lstick_position = Arc::new(Mutex::new(position_vector.clone()));
        let rstick_position = Arc::new(Mutex::new(position_vector.clone()));
        let cursor_movement = Arc::new(Mutex::new((0, 0)));
        let scroll_movement = Arc::new(Mutex::new((0, 0)));
        let device_is_connected: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));
        let active_layout: Arc<Mutex<u16>> = Arc::new(Mutex::new(0));
        let paused: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
        let last_action: Arc<Mutex<Option<LastAction>>> = Arc::new(Mutex::new(None));
        let held_keys: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        let current_config: Arc<Mutex<Config>> = Arc::new(Mutex::new(
            config
                .iter()
                .find(|&x| x.associations == Associations::default())
                .unwrap()
                .clone(),
        ));
        let lstick_function = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("LSTICK")
            .unwrap_or(&"cursor".to_string())
            .to_string();
        let lstick_sensitivity: u64 = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("LSTICK_SENSITIVITY")
            .unwrap_or(&"0".to_string())
            .parse::<u64>()
            .expect("Invalid value for LSTICK_SENSITIVITY, please use an integer value >= 0");
        let lstick_deadzone: i32 = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("LSTICK_DEADZONE")
            .unwrap_or(&"5".to_string())
            .parse::<i32>()
            .expect("Invalid value for LSTICK_DEADZONE, please use an integer between 0 and 128.");
        let lstick_activation_modifiers: Vec<Event> = parse_modifiers(
            &config
                .iter()
                .find(|&x| x.associations == Associations::default())
                .unwrap()
                .settings,
            "LSTICK_ACTIVATION_MODIFIERS",
        );
        let lstick = Stick {
            function: lstick_function,
            sensitivity: lstick_sensitivity,
            deadzone: lstick_deadzone,
            activation_modifiers: lstick_activation_modifiers,
        };

        let rstick_function: String = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("RSTICK")
            .unwrap_or(&"scroll".to_string())
            .to_string();
        let rstick_sensitivity: u64 = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("RSTICK_SENSITIVITY")
            .unwrap_or(&"0".to_string())
            .parse::<u64>()
            .expect("Invalid value for RSTICK_SENSITIVITY, please use an integer value >= 0");
        let rstick_deadzone: i32 = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("RSTICK_DEADZONE")
            .unwrap_or(&"5".to_string())
            .parse::<i32>()
            .expect("Invalid value for RSTICK_DEADZONE, please use an integer between 0 and 128.");
        let rstick_activation_modifiers: Vec<Event> = parse_modifiers(
            &config
                .iter()
                .find(|&x| x.associations == Associations::default())
                .unwrap()
                .settings,
            "RSTICK_ACTIVATION_MODIFIERS",
        );
        let rstick = Stick {
            function: rstick_function,
            sensitivity: rstick_sensitivity,
            deadzone: rstick_deadzone,
            activation_modifiers: rstick_activation_modifiers,
        };

        let axis_16_bit: bool = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("16_BIT_AXIS")
            .unwrap_or(&"false".to_string())
            .parse()
            .expect("16_BIT_AXIS can only be true or false.");

        let stadia: bool = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("STADIA")
            .unwrap_or(&"false".to_string())
            .parse()
            .expect("STADIA can only be true or false.");

        let chain_only: bool = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("CHAIN_ONLY")
            .unwrap_or(&"true".to_string())
            .parse()
            .expect("CHAIN_ONLY can only be true or false.");

        let invert_cursor_axis: bool = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("INVERT_CURSOR_AXIS")
            .unwrap_or(&"false".to_string())
            .parse()
            .expect("INVERT_CURSOR_AXIS can only be true or false.");

        let invert_scroll_axis: bool = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("INVERT_SCROLL_AXIS")
            .unwrap_or(&"false".to_string())
            .parse()
            .expect("INVERT_SCROLL_AXIS can only be true or false.");

        let cursor_speed: i32 = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("CURSOR_SPEED")
            .unwrap_or(&"0".to_string())
            .parse()
            .expect("Invalid value for CURSOR_SPEED, please use an integer value.");

        let cursor_acceleration: f32 = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("CURSOR_ACCEL")
            .unwrap_or(&"1".to_string())
            .parse()
            .expect("Invalid value for CURSOR_ACCEL, please use an float value between 0 and 1.");

        let scroll_speed: i32 = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("SCROLL_SPEED")
            .unwrap_or(&"0".to_string())
            .parse()
            .expect("Invalid value for SCROLL_SPEED, please use an integer value.");

        let scroll_acceleration: f32 = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("SCROLL_ACCEL")
            .unwrap_or(&"1".to_string())
            .parse()
            .expect("Invalid value for SCROLL_ACCEL, please use a float value between 0 and 1.");

        let cursor = Movement {
            speed: cursor_speed,
            acceleration: cursor_acceleration,
        };

        let scroll = Movement {
            speed: scroll_speed,
            acceleration: scroll_acceleration,
        };
        let layout_switcher = if let Some(combination) = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("LAYOUT_SWITCHER")
        {
            if let Some(sequence) = combination.rsplit_once("-") {
                let mut mods: Vec<Event> = sequence
                    .0
                    .split("-")
                    .map(|m| Event::Key(Key::from_str(m).expect("LAYOUT_SWITCHER is invalid.")))
                    .collect();
                mods.sort();
                mods.dedup();
                Some((
                    Event::Key(Key::from_str(sequence.1).expect("LAYOUT_SWITCHER is invalid.")),
                    mods,
                ))
            } else {
                Some((
                    Event::Key(Key::from_str(combination).expect("LAYOUT_SWITCHER is invalid.")),
                    Vec::new(),
                ))
            }
        } else {
            None
        };
        let notify_layout_switch: bool = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("NOTIFY_LAYOUT_SWITCH")
            .unwrap_or(&"false".to_string())
            .parse()
            .expect("NOTIFY_LAYOUT_SWITCH can only be true or false.");

        let cursor_when_paused: bool = config
            .iter()
            .find(|&x| x.associations == Associations::default())
            .unwrap()
            .settings
            .get("CURSOR_WHEN_PAUSED")
            .unwrap_or(&"false".to_string())
            .parse()
            .expect("CURSOR_WHEN_PAUSED can only be true or false.");

        let settings = Settings {
            lstick,
            rstick,
            invert_cursor_axis,
            invert_scroll_axis,
            axis_16_bit,
            stadia,
            cursor,
            scroll,
            chain_only,
            layout_switcher,
            notify_layout_switch,
            cursor_when_paused,
        };
        Self {
            config,
            stream,
            virt_dev,
            lstick_position,
            rstick_position,
            cursor_movement,
            scroll_movement,
            modifiers,
            modifier_was_activated,
            paused,
            last_action,
            held_keys,
            device_is_connected,
            device_error_notify,
            active_layout,
            current_config,
            environment,
            settings,
            active_client,
            window_changed,
        }
    }

    pub async fn start(&self) {
        println!(
            "{:?} detected, reading events.\n",
            self.config
                .iter()
                .find(|&x| x.associations == Associations::default())
                .unwrap()
                .name
        );
        self.write_state().await;
        self.start_control_socket().await;
        tokio::join!(
            self.event_loop(),
            self.cursor_loop(),
            self.scroll_loop(),
            self.key_cursor_loop(),
            self.key_scroll_loop(),
            self.window_changed_loop(),
        );
    }

    /// Listens for window-activation changes fired by kwin_watcher (KDE only).
    /// For other compositors this future returns immediately.
    async fn window_changed_loop(&self) {
        if let Server::Connected(s) = &self.environment.server {
            if s == "KDE" {
                loop {
                    self.window_changed.notified().await;
                    self.update_config().await;
                }
            }
        }
    }

    pub async fn event_loop(&self) {
        let (
            mut dpad_values,
            mut lstick_values,
            mut rstick_values,
            mut triggers_values,
            mut abs_wheel_position,
        ) = ((0, 0), (0, 0), (0, 0), (0, 0), 0);
        let mut stream = self.stream.lock().await;
        let mut pen_events: Vec<InputEvent> = Vec::new();
        let is_tablet: bool = stream
            .device()
            .supported_keys()
            .unwrap_or(&evdev::AttributeSet::new())
            .contains(Key::BTN_TOOL_PEN);
        let mut max_abs_wheel = 0;
        if let Ok(abs_state) = stream.device().get_abs_state() {
            for state in abs_state {
                if state.maximum > max_abs_wheel {
                    max_abs_wheel = state.maximum;
                }
            }
        }
        let mut had_device_error = false;
        while let Some(event_result) = stream.next().await {
            let event = match event_result {
                Ok(e) => e,
                Err(e) => {
                    println!(
                        "[makima] Device read error on \"{}\": {} — signaling reconnect.",
                        self.current_config.lock().await.name,
                        e
                    );
                    had_device_error = true;
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
                    _ => {
                        self.convert_event(
                            event,
                            Event::Key(Key(event.code())),
                            event.value(),
                            false,
                        )
                        .await
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
                (_, _, AbsoluteAxisType::ABS_HAT0X, _) => {
                    match event.value() {
                        -1 => {
                            self.convert_event(event, Event::Axis(Axis::BTN_DPAD_LEFT), 1, false)
                                .await;
                            dpad_values.0 = -1;
                        }
                        1 => {
                            self.convert_event(event, Event::Axis(Axis::BTN_DPAD_RIGHT), 1, false)
                                .await;
                            dpad_values.0 = 1;
                        }
                        0 => {
                            match dpad_values.0 {
                                -1 => {
                                    self.convert_event(
                                        event,
                                        Event::Axis(Axis::BTN_DPAD_LEFT),
                                        0,
                                        false,
                                    )
                                    .await
                                }
                                1 => {
                                    self.convert_event(
                                        event,
                                        Event::Axis(Axis::BTN_DPAD_RIGHT),
                                        0,
                                        false,
                                    )
                                    .await
                                }
                                _ => {}
                            }
                            dpad_values.0 = 0;
                        }
                        _ => {}
                    };
                }
                (_, _, AbsoluteAxisType::ABS_HAT0Y, _) => {
                    match event.value() {
                        -1 => {
                            self.convert_event(event, Event::Axis(Axis::BTN_DPAD_UP), 1, false)
                                .await;
                            dpad_values.1 = -1;
                        }
                        1 => {
                            self.convert_event(event, Event::Axis(Axis::BTN_DPAD_DOWN), 1, false)
                                .await;
                            dpad_values.1 = 1;
                        }
                        0 => {
                            match dpad_values.1 {
                                -1 => {
                                    self.convert_event(
                                        event,
                                        Event::Axis(Axis::BTN_DPAD_UP),
                                        0,
                                        false,
                                    )
                                    .await
                                }
                                1 => {
                                    self.convert_event(
                                        event,
                                        Event::Axis(Axis::BTN_DPAD_DOWN),
                                        0,
                                        false,
                                    )
                                    .await
                                }
                                _ => {}
                            }
                            dpad_values.1 = 0;
                        }
                        _ => {}
                    };
                }
                (
                    EventType::ABSOLUTE,
                    _,
                    AbsoluteAxisType::ABS_X | AbsoluteAxisType::ABS_Y,
                    false,
                ) => match self.settings.lstick.function.as_str() {
                    "cursor" | "scroll" => {
                        let axis_value = self
                            .get_axis_value(&event, &self.settings.lstick.deadzone)
                            .await;
                        let mut lstick_position = self.lstick_position.lock().await;
                        lstick_position[event.code() as usize] = axis_value;
                    }
                    "bind" => {
                        let axis_value = self
                            .get_axis_value(&event, &self.settings.lstick.deadzone)
                            .await;
                        let clamped_value = if axis_value < 0 {
                            -1
                        } else if axis_value > 0 {
                            1
                        } else {
                            0
                        };
                        match AbsoluteAxisType(event.code()) {
                            AbsoluteAxisType::ABS_Y => match clamped_value {
                                -1 if lstick_values.1 != -1 => {
                                    self.convert_event(
                                        event,
                                        Event::Axis(Axis::LSTICK_UP),
                                        1,
                                        false,
                                    )
                                    .await;
                                    lstick_values.1 = -1
                                }
                                1 if lstick_values.1 != 1 => {
                                    self.convert_event(
                                        event,
                                        Event::Axis(Axis::LSTICK_DOWN),
                                        1,
                                        false,
                                    )
                                    .await;
                                    lstick_values.1 = 1
                                }
                                0 => {
                                    if lstick_values.1 != 0 {
                                        match lstick_values.1 {
                                            -1 => {
                                                self.convert_event(
                                                    event,
                                                    Event::Axis(Axis::LSTICK_UP),
                                                    0,
                                                    false,
                                                )
                                                .await
                                            }
                                            1 => {
                                                self.convert_event(
                                                    event,
                                                    Event::Axis(Axis::LSTICK_DOWN),
                                                    0,
                                                    false,
                                                )
                                                .await
                                            }
                                            _ => {}
                                        }
                                        lstick_values.1 = 0;
                                    }
                                }
                                _ => {}
                            },
                            AbsoluteAxisType::ABS_X => match clamped_value {
                                -1 if lstick_values.0 != -1 => {
                                    self.convert_event(
                                        event,
                                        Event::Axis(Axis::LSTICK_LEFT),
                                        1,
                                        false,
                                    )
                                    .await;
                                    lstick_values.0 = -1
                                }
                                1 => {
                                    if lstick_values.0 != 1 {
                                        self.convert_event(
                                            event,
                                            Event::Axis(Axis::LSTICK_RIGHT),
                                            1,
                                            false,
                                        )
                                        .await;
                                        lstick_values.0 = 1
                                    }
                                }
                                0 => {
                                    if lstick_values.0 != 0 {
                                        match lstick_values.0 {
                                            -1 => {
                                                self.convert_event(
                                                    event,
                                                    Event::Axis(Axis::LSTICK_LEFT),
                                                    0,
                                                    false,
                                                )
                                                .await
                                            }
                                            1 => {
                                                self.convert_event(
                                                    event,
                                                    Event::Axis(Axis::LSTICK_RIGHT),
                                                    0,
                                                    false,
                                                )
                                                .await
                                            }
                                            _ => {}
                                        }
                                        lstick_values.0 = 0;
                                    }
                                }
                                _ => {}
                            },
                            _ => {}
                        }
                    }
                    _ => {}
                },
                (
                    EventType::ABSOLUTE,
                    _,
                    AbsoluteAxisType::ABS_RX | AbsoluteAxisType::ABS_RY,
                    false,
                ) => match self.settings.rstick.function.as_str() {
                    "cursor" | "scroll" => {
                        let axis_value = self
                            .get_axis_value(&event, &self.settings.rstick.deadzone)
                            .await;
                        let mut rstick_position = self.rstick_position.lock().await;
                        rstick_position[event.code() as usize - 3] = axis_value;
                    }
                    "bind" => {
                        let axis_value = self
                            .get_axis_value(&event, &self.settings.rstick.deadzone)
                            .await;
                        let clamped_value = if axis_value < 0 {
                            -1
                        } else if axis_value > 0 {
                            1
                        } else {
                            0
                        };
                        match AbsoluteAxisType(event.code()) {
                            AbsoluteAxisType::ABS_RY => match clamped_value {
                                -1 => {
                                    if rstick_values.1 != -1 {
                                        self.convert_event(
                                            event,
                                            Event::Axis(Axis::RSTICK_UP),
                                            1,
                                            false,
                                        )
                                        .await;
                                        rstick_values.1 = -1
                                    }
                                }
                                1 => {
                                    if rstick_values.1 != 1 {
                                        self.convert_event(
                                            event,
                                            Event::Axis(Axis::RSTICK_DOWN),
                                            1,
                                            false,
                                        )
                                        .await;
                                        rstick_values.1 = 1
                                    }
                                }
                                0 => {
                                    if rstick_values.1 != 0 {
                                        match rstick_values.1 {
                                            -1 => {
                                                self.convert_event(
                                                    event,
                                                    Event::Axis(Axis::RSTICK_UP),
                                                    0,
                                                    false,
                                                )
                                                .await
                                            }
                                            1 => {
                                                self.convert_event(
                                                    event,
                                                    Event::Axis(Axis::RSTICK_DOWN),
                                                    0,
                                                    false,
                                                )
                                                .await
                                            }
                                            _ => {}
                                        }
                                        rstick_values.1 = 0;
                                    }
                                }
                                _ => {}
                            },
                            AbsoluteAxisType::ABS_RX => match clamped_value {
                                -1 if rstick_values.0 != -1 => {
                                    self.convert_event(
                                        event,
                                        Event::Axis(Axis::RSTICK_LEFT),
                                        1,
                                        false,
                                    )
                                    .await;
                                    rstick_values.0 = -1
                                }
                                1 => {
                                    if rstick_values.0 != 1 {
                                        self.convert_event(
                                            event,
                                            Event::Axis(Axis::RSTICK_RIGHT),
                                            1,
                                            false,
                                        )
                                        .await;
                                        rstick_values.0 = 1
                                    }
                                }
                                0 => {
                                    if rstick_values.0 != 0 {
                                        match rstick_values.0 {
                                            -1 => {
                                                self.convert_event(
                                                    event,
                                                    Event::Axis(Axis::RSTICK_LEFT),
                                                    0,
                                                    false,
                                                )
                                                .await
                                            }
                                            1 => {
                                                self.convert_event(
                                                    event,
                                                    Event::Axis(Axis::RSTICK_RIGHT),
                                                    0,
                                                    false,
                                                )
                                                .await
                                            }
                                            _ => {}
                                        }
                                        rstick_values.0 = 0;
                                    }
                                }
                                _ => {}
                            },
                            _ => {}
                        }
                    }
                    _ => {}
                },
                (EventType::ABSOLUTE, _, AbsoluteAxisType::ABS_Z, false) => {
                    if !self.settings.stadia {
                        match (event.value(), triggers_values.0) {
                            (0, 1) => {
                                self.convert_event(event, Event::Axis(Axis::ABS_Z), 0, false)
                                    .await;
                                triggers_values.0 = 0;
                            }
                            (_, 0) => {
                                self.convert_event(event, Event::Axis(Axis::ABS_Z), 1, false)
                                    .await;
                                triggers_values.0 = 1;
                            }
                            _ => {}
                        }
                    } else {
                        match self.settings.rstick.function.as_str() {
                            "cursor" | "scroll" => {
                                let axis_value = self
                                    .get_axis_value(&event, &self.settings.rstick.deadzone)
                                    .await;
                                let mut rstick_position = self.rstick_position.lock().await;
                                rstick_position[0] = axis_value;
                            }
                            "bind" => {
                                let axis_value = self
                                    .get_axis_value(&event, &self.settings.rstick.deadzone)
                                    .await;
                                let clamped_value = if axis_value < 0 {
                                    -1
                                } else if axis_value > 0 {
                                    1
                                } else {
                                    0
                                };
                                match clamped_value {
                                    -1 if rstick_values.0 != -1 => {
                                        self.convert_event(
                                            event,
                                            Event::Axis(Axis::RSTICK_LEFT),
                                            1,
                                            false,
                                        )
                                        .await;
                                        rstick_values.0 = -1
                                    }
                                    1 => {
                                        if rstick_values.0 != 1 {
                                            self.convert_event(
                                                event,
                                                Event::Axis(Axis::RSTICK_RIGHT),
                                                1,
                                                false,
                                            )
                                            .await;
                                            rstick_values.0 = 1
                                        }
                                    }
                                    0 => {
                                        if rstick_values.0 != 0 {
                                            match rstick_values.0 {
                                                -1 => {
                                                    self.convert_event(
                                                        event,
                                                        Event::Axis(Axis::RSTICK_LEFT),
                                                        0,
                                                        false,
                                                    )
                                                    .await
                                                }
                                                1 => {
                                                    self.convert_event(
                                                        event,
                                                        Event::Axis(Axis::RSTICK_RIGHT),
                                                        0,
                                                        false,
                                                    )
                                                    .await
                                                }
                                                _ => {}
                                            }
                                            rstick_values.0 = 0;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                }
                (EventType::ABSOLUTE, _, AbsoluteAxisType::ABS_RZ, false) => {
                    if !self.settings.stadia {
                        match (event.value(), triggers_values.1) {
                            (0, 1) => {
                                self.convert_event(event, Event::Axis(Axis::ABS_RZ), 0, false)
                                    .await;
                                triggers_values.1 = 0;
                            }
                            (_, 0) => {
                                self.convert_event(event, Event::Axis(Axis::ABS_RZ), 1, false)
                                    .await;
                                triggers_values.1 = 1;
                            }
                            _ => {}
                        }
                    } else {
                        match self.settings.rstick.function.as_str() {
                            "cursor" | "scroll" => {
                                let axis_value = self
                                    .get_axis_value(&event, &self.settings.rstick.deadzone)
                                    .await;
                                let mut rstick_position = self.rstick_position.lock().await;
                                rstick_position[1] = axis_value;
                            }
                            "bind" => {
                                let axis_value = self
                                    .get_axis_value(&event, &self.settings.rstick.deadzone)
                                    .await;
                                let clamped_value = if axis_value < 0 {
                                    -1
                                } else if axis_value > 0 {
                                    1
                                } else {
                                    0
                                };
                                match clamped_value {
                                    -1 => {
                                        if rstick_values.1 != -1 {
                                            self.convert_event(
                                                event,
                                                Event::Axis(Axis::RSTICK_UP),
                                                1,
                                                false,
                                            )
                                            .await;
                                            rstick_values.1 = -1
                                        }
                                    }
                                    1 => {
                                        if rstick_values.1 != 1 {
                                            self.convert_event(
                                                event,
                                                Event::Axis(Axis::RSTICK_DOWN),
                                                1,
                                                false,
                                            )
                                            .await;
                                            rstick_values.1 = 1
                                        }
                                    }
                                    0 => {
                                        if rstick_values.1 != 0 {
                                            match rstick_values.1 {
                                                -1 => {
                                                    self.convert_event(
                                                        event,
                                                        Event::Axis(Axis::RSTICK_UP),
                                                        0,
                                                        false,
                                                    )
                                                    .await
                                                }
                                                1 => {
                                                    self.convert_event(
                                                        event,
                                                        Event::Axis(Axis::RSTICK_DOWN),
                                                        0,
                                                        false,
                                                    )
                                                    .await
                                                }
                                                _ => {}
                                            }
                                            rstick_values.1 = 0;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                }
                (EventType::MISC, _, _, true) => {
                    if evdev::MiscType(event.code()) == evdev::MiscType::MSC_SERIAL {
                        pen_events.push(event);
                        let mut virt_dev = self.virt_dev.lock().await;
                        virt_dev.abs.emit(&pen_events).unwrap();
                        pen_events.clear()
                    }
                }
                _ => self.emit_default_event(event).await,
            }
        }
        if had_device_error {
            self.device_error_notify.notify_one();
        }
        let mut device_is_connected = self.device_is_connected.lock().await;
        *device_is_connected = false;

        println!(
            "Disconnected device \"{}\".\n",
            self.current_config.lock().await.name
        );
    }
    async fn convert_event(
        &self,
        default_event: InputEvent,
        event: Event,
        value: i32,
        send_zero: bool,
    ) {

        // Track all currently held buttons so the HUD can highlight them.
        // Only KEY events are tracked (not axis/stick movements).
        if matches!(event, Event::Key(_)) {
            let mut held = self.held_keys.lock().await;
            match value {
                1 => { held.push(event); held.sort(); held.dedup(); }
                0 => held.retain(|&x| x != event),
                _ => {}
            }
        } // lock dropped before any await

        if value == 1 {
            self.update_config().await;
        };
        let config = self.current_config.lock().await.clone();
        let paused = *self.paused.lock().await;
        let mut modifiers = self.modifiers.lock().await.clone();
        modifiers.sort();
        modifiers.dedup();


        // ── Custom modifier + remap intercept ──────────────────────────────────
        // A button that is BOTH in CUSTOM_MODIFIERS and has a base remap entry
        // acts as two things simultaneously:
        //   1. It emits the remap output (e.g. KEY_LEFTCTRL) as a held system
        //      key, so Ctrl+click etc. work natively while the button is held.
        //   2. It tracks the INPUT key (e.g. BTN_TL) in self.modifiers, so
        //      combo entries keyed on BTN_TL are found correctly.
        // Without this intercept makima would track the OUTPUT key (KEY_LEFTCTRL)
        // instead of the input key, causing combo lookups to never match.
        if config.mapped_modifiers.custom.contains(&event) {
            if let Some(out_keys) = config.bindings.remap
                .get(&event)
                .and_then(|m| m.get(&vec![]))
                .cloned()
            {
                {
                    if !paused {
                        let mut virt_dev = self.virt_dev.lock().await;
                        for key in &out_keys {
                            virt_dev.keys.emit(&[
                                InputEvent::new_now(EventType::KEY, key.code(), value),
                            ]).unwrap();
                        }
                    }
                }
                self.set_last_emitted(&event, &out_keys, value, false, config.bindings.labels.get(&(event, vec![])).cloned()).await;
                self.toggle_modifiers(event, value, &config).await;
                return;
            }
        }
        // ───────────────────────────────────────────────────────────────────────

        if let Some(map) = config.bindings.remap.get(&event) {
            if let Some(event_list) = map.get(&modifiers) {
                self.set_last_emitted(&event, event_list, value, !modifiers.is_empty(), config.bindings.labels.get(&(event, modifiers.clone())).cloned()).await;
                self.emit_event(
                    event_list,
                    value,
                    &modifiers,
                    &config,
                    modifiers.is_empty(),
                    !modifiers.is_empty(),
                )
                .await;
                if send_zero {
                    let mut modifiers = self.modifiers.lock().await.clone();
                    modifiers.sort();
                    modifiers.dedup();
                    self.emit_event(
                        event_list,
                        0,
                        &modifiers,
                        &config,
                        modifiers.is_empty(),
                        !modifiers.is_empty(),
                    )
                    .await;
                }
                return;
            }
            if let Some(event_list) = map.get(&vec![Event::Hold]) {
                if !modifiers.is_empty() || self.settings.chain_only == false {
                    self.emit_event(event_list, value, &modifiers, &config, false, false)
                        .await;
                    return;
                }
            }
            if let Some(map) = config.bindings.commands.get(&event) {
                if let Some(command_list) = map.get(&modifiers) {
                    let is_no_pause = config.bindings.no_pause.contains(&(event, modifiers.clone()))
                        || config.bindings.no_pause.contains(&(event, vec![]));
                    if value == 1 {
                        self.set_last_emitted_cmd(&event, command_list, config.bindings.labels.get(&(event, modifiers.clone())).cloned()).await;
                        if !paused || is_no_pause {
                            self.spawn_subprocess(command_list).await
                        }
                    } else {
                        self.write_state().await;
                    }
                    return;
                }
            }
            if let Some(map) = config.bindings.movements.get(&event) {
                if let Some(movement) = map.get(&modifiers) {
                    if value <= 1 {
                        self.emit_movement(movement, value).await;
                    }
                    return;
                };
            }
            if let Some(event_list) = map.get(&Vec::new()) {
                self.set_last_emitted(&event, event_list, value, false, config.bindings.labels.get(&(event, vec![])).cloned()).await;
                // Fallback to base binding: no combo defined for the current modifier set.
                // Do NOT release the held modifier keys — the modifier is already held at
                // system level and should stay held (e.g. L1+A → Ctrl+Enter, not Enter).
                self.emit_event(event_list, value, &modifiers, &config, false, false)
                    .await;
                if send_zero {
                    let mut modifiers = self.modifiers.lock().await.clone();
                    modifiers.sort();
                    modifiers.dedup();
                    self.emit_event(event_list, 0, &modifiers, &config, false, false)
                        .await;
                }
                return;
            }
        }
        if let Some(map) = config.bindings.commands.get(&event) {
            if let Some(command_list) = map.get(&modifiers) {
                let is_no_pause = config.bindings.no_pause.contains(&(event, modifiers.clone()))
                    || config.bindings.no_pause.contains(&(event, vec![]));
                if value == 1 {
                    self.set_last_emitted_cmd(&event, command_list, config.bindings.labels.get(&(event, modifiers.clone())).cloned()).await;
                    if !paused || is_no_pause {
                        self.spawn_subprocess(command_list).await
                    }
                } else {
                    self.write_state().await;
                }
                return;
            }
        }
        if let Some(map) = config.bindings.movements.get(&event) {
            if let Some(movement) = map.get(&modifiers) {
                if value <= 1 {
                    if !paused {
                        self.emit_movement(movement, value).await;
                    }
                }
                return;
            };
        }
        if let Some(map) = &self.settings.layout_switcher {
            if map.0 == event && map.1 == modifiers && value == 1 {
                let mut virt_dev = self.virt_dev.lock().await;
                for modifier in modifiers {
                    self.toggle_modifiers(modifier, 0, &config).await;
                    if let Event::Key(key) = modifier {
                        let virtual_event: InputEvent =
                            InputEvent::new_now(EventType::KEY, key.code(), 0);
                        virt_dev.keys.emit(&[virtual_event]).unwrap()
                    }
                }
                if let Event::Key(key) = event {
                    let virtual_event: InputEvent =
                        InputEvent::new_now(EventType::KEY, key.code(), 0);
                    virt_dev.keys.emit(&[virtual_event]).unwrap()
                }
                self.change_active_layout().await;
                return;
            }
        }
        self.emit_nonmapped_event(default_event, event, value, &modifiers, &config)
            .await;
    }

    async fn emit_event(
        &self,
        event_list: &Vec<Key>,
        value: i32,
        modifiers: &Vec<Event>,
        config: &Config,
        release_keys: bool,
        ignore_modifiers: bool,
    ) {
        let paused = *self.paused.lock().await;
        if paused {
            return;
        }
        let mut virt_dev = self.virt_dev.lock().await;
        let mut modifier_was_activated = self.modifier_was_activated.lock().await;
        if release_keys && value != 2 {
            let released_keys: Vec<Key> = self.released_keys(&modifiers, &config).await;
            for key in released_keys {
                if config.mapped_modifiers.all.contains(&Event::Key(key)) {
                    self.toggle_modifiers(Event::Key(key), 0, &config).await;
                    let virtual_event: InputEvent =
                        InputEvent::new_now(EventType::KEY, key.code(), 0);
                    virt_dev.keys.emit(&[virtual_event]).unwrap();
                }
            }
        } else if ignore_modifiers {
            // For each active modifier, release its OUTPUT key (not the raw input
            // code). For a custom modifier with a remap (e.g. BTN_TL → KEY_LEFTCTRL)
            // that means releasing KEY_LEFTCTRL so the system modifier state is
            // cleared before the combo keys are emitted.
            for modifier in modifiers.iter() {
                let codes: Vec<u16> = config
                    .bindings
                    .remap
                    .get(modifier)
                    .and_then(|m| m.get(&vec![]))
                    .map(|keys| keys.iter().map(|k| k.code()).collect())
                    .unwrap_or_else(|| match modifier {
                        Event::Key(k) => vec![k.code()],
                        _ => vec![],
                    });
                for code in codes {
                    virt_dev.keys.emit(&[
                        InputEvent::new_now(EventType::KEY, code, 0),
                    ]).unwrap();
                }
            }
        }
        for key in event_list {
            if release_keys && value != 2 {
                self.toggle_modifiers(Event::Key(*key), value, &config)
                    .await;
            }
            if config.mapped_modifiers.custom.contains(&Event::Key(*key)) {
                if value == 0 && !*modifier_was_activated {
                    let virtual_event: InputEvent =
                        InputEvent::new_now(EventType::KEY, key.code(), 1);
                    virt_dev.keys.emit(&[virtual_event]).unwrap();
                    let virtual_event: InputEvent =
                        InputEvent::new_now(EventType::KEY, key.code(), 0);
                    virt_dev.keys.emit(&[virtual_event]).unwrap();
                    *modifier_was_activated = true;
                } else if value == 1 {
                    *modifier_was_activated = false;
                }
            } else {
                let virtual_event: InputEvent =
                    InputEvent::new_now(EventType::KEY, key.code(), value);
                virt_dev.keys.emit(&[virtual_event]).unwrap();
                *modifier_was_activated = true;
            }
        }
    }

    async fn emit_nonmapped_event(
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
        let mut virt_dev = self.virt_dev.lock().await;
        let mut modifier_was_activated = self.modifier_was_activated.lock().await;
        if config.mapped_modifiers.all.contains(&event) && value != 2 {
            let released_keys: Vec<Key> = self.released_keys(&modifiers, &config).await;
            for key in released_keys {
                self.toggle_modifiers(Event::Key(key), 0, &config).await;
                let virtual_event: InputEvent = InputEvent::new_now(EventType::KEY, key.code(), 0);
                virt_dev.keys.emit(&[virtual_event]).unwrap()
            }
        }
        self.toggle_modifiers(event, value, &config).await;
        if config.mapped_modifiers.custom.contains(&event) {
            if value == 0 && !*modifier_was_activated {
                let virtual_event: InputEvent =
                    InputEvent::new_now(default_event.event_type(), default_event.code(), 1);
                virt_dev.keys.emit(&[virtual_event]).unwrap();
                let virtual_event: InputEvent =
                    InputEvent::new_now(default_event.event_type(), default_event.code(), 0);
                virt_dev.keys.emit(&[virtual_event]).unwrap();
                *modifier_was_activated = true;
            } else if value == 1 {
                *modifier_was_activated = false;
            }
        } else {
            *modifier_was_activated = true;
            match default_event.event_type() {
                EventType::KEY => {
                    virt_dev.keys.emit(&[default_event]).unwrap();
                }
                EventType::RELATIVE => {
                    virt_dev.axis.emit(&[default_event]).unwrap();
                }
                EventType::ABSOLUTE => {
                    virt_dev.abs.emit(&[default_event]).unwrap();
                }
                EventType::MISC => {
                    let mut virt_dev = self.virt_dev.lock().await;
                    virt_dev.abs.emit(&[default_event]).unwrap();
                }
                _ => {}
            }
        }
    }

    async fn emit_default_event(&self, event: InputEvent) {
        if *self.paused.lock().await {
            return;
        }
        match event.event_type() {
            EventType::KEY => {
                let mut virt_dev = self.virt_dev.lock().await;
                virt_dev.keys.emit(&[event]).unwrap();
            }
            EventType::RELATIVE => {
                let mut virt_dev = self.virt_dev.lock().await;
                virt_dev.axis.emit(&[event]).unwrap();
            }
            EventType::ABSOLUTE => {
                let mut virt_dev = self.virt_dev.lock().await;
                virt_dev.abs.emit(&[event]).unwrap();
            }
            EventType::MISC => {
                let mut virt_dev = self.virt_dev.lock().await;
                virt_dev.abs.emit(&[event]).unwrap();
            }
            _ => {}
        }
    }

    async fn emit_movement(&self, movement: &Relative, value: i32) {
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

    async fn spawn_subprocess(&self, command_list: &Vec<String>) {
        let mut modifier_was_activated = self.modifier_was_activated.lock().await;
        *modifier_was_activated = true;
        let (user, running_as_root) = if let Ok(sudo_user) = &self.environment.sudo_user {
            (Option::Some(sudo_user), true)
        } else if let Ok(user) = &self.environment.user {
            (Option::Some(user), false)
        } else {
            (Option::None, false)
        };
        if let Some(user) = user {
            for command in command_list {
                if running_as_root {
                    match fork() {
                        Ok(Fork::Child) => match fork() {
                            Ok(Fork::Child) => {
                                setsid().unwrap();
                                Command::new("runuser")
                                    .args([user, "-c", command])
                                    .stdin(Stdio::null())
                                    .stdout(Stdio::null())
                                    .stderr(Stdio::null())
                                    .spawn()
                                    .unwrap();
                                std::process::exit(0);
                            }
                            Ok(Fork::Parent(_)) => std::process::exit(0),
                            Err(_) => std::process::exit(1),
                        },
                        Ok(Fork::Parent(_)) => (),
                        Err(_) => std::process::exit(1),
                    }
                } else {
                    Command::new("sh")
                        .arg("-c")
                        .arg(format!(
                            "systemd-run --wait --pipe --user --machine {}@ -- systemd-run --user --scope {}",
                            user, command
                        ))
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .unwrap();
                }
            }
        }
    }

    async fn get_axis_value(&self, event: &InputEvent, deadzone: &i32) -> i32 {
        let distance_from_center: i32 = match self.settings.axis_16_bit {
            false => (event.value() as i32 - 128) * 200,
            _ => event.value() as i32,
        };
        if distance_from_center.abs() <= deadzone * 200 {
            0
        } else {
            (distance_from_center + 2000 - 1) / 2000
        }
    }

    async fn toggle_modifiers(&self, modifier: Event, value: i32, config: &Config) {
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

    async fn released_keys(&self, modifiers: &Vec<Event>, config: &Config) -> Vec<Key> {
        let mut released_keys: Vec<Key> = Vec::new();
        for (_key, hashmap) in config.bindings.remap.iter() {
            if let Some(event_list) = hashmap.get(modifiers) {
                released_keys.extend(event_list);
            }
        }
        released_keys
    }

    async fn change_active_layout(&self) {
        let mut active_layout = self.active_layout.lock().await;
        let active_window = get_active_window(&self.environment, &self.config).await;
        loop {
            if *active_layout == 3 {
                *active_layout = 0
            } else {
                *active_layout += 1
            };
            if let Some(_) = self.config.iter().find(|&x| {
                x.associations.layout == *active_layout && x.associations.client == active_window
            }) {
                break;
            };
        }
        if self.settings.notify_layout_switch {
            let notify = vec![String::from(format!(
                "notify-send -t 500 'Makima' 'Switching to layout {}'",
                *active_layout
            ))];
            self.spawn_subprocess(&notify).await;
        }
    }

    fn update_config(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let active_layout = self.active_layout.lock().await.clone();
            // For KDE: the window class is maintained by the kwin_watcher task
            // (event-driven, no subprocess). For all other compositors: call
            // get_active_window() on every button-press as before.
            let active_window = match &self.environment.server {
                Server::Connected(s) if s == "KDE" => {
                    let client = self.active_client.lock().await.clone();
                    if self.config.iter().any(|x| x.associations.client == client) {
                        client
                    } else {
                        Client::Default
                    }
                }
                _ => get_active_window(&self.environment, &self.config).await,
            };
            let associations = Associations {
                client: active_window,
                layout: active_layout,
            };
            match self.config.iter().find(|&x| x.associations == associations) {
                Some(config) => {
                    let mut current_config = self.current_config.lock().await;
                    *current_config = config.clone();
                }
                None => {
                    self.change_active_layout().await;
                    self.update_config().await;
                }
            };
            self.write_state().await;
        })
    }

    async fn write_state(&self) {
        // Each lock acquisition is guarded by a timeout so that a deadlock
        // (same task trying to re-acquire a lock it already holds) surfaces
        // immediately in the journal instead of silently freezing makima.
        const TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

        let config = match tokio::time::timeout(TIMEOUT, self.current_config.lock()).await {
            Ok(guard) => guard.clone(),
            Err(_) => {
                eprintln!("makima: write_state: current_config lock timed out — possible deadlock");
                return;
            }
        };
        let modifiers = match tokio::time::timeout(TIMEOUT, self.modifiers.lock()).await {
            Ok(guard) => guard.clone(),
            Err(_) => {
                eprintln!("makima: write_state: modifiers lock timed out — possible deadlock");
                return;
            }
        };
        let layout = match tokio::time::timeout(TIMEOUT, self.active_layout.lock()).await {
            Ok(guard) => *guard,
            Err(_) => {
                eprintln!("makima: write_state: active_layout lock timed out — possible deadlock");
                return;
            }
        };

        let paused = match tokio::time::timeout(TIMEOUT, self.paused.lock()).await {
            Ok(guard) => *guard,
            Err(_) => {
                eprintln!("makima: write_state: paused lock timed out — possible deadlock");
                return;
            }
        };
        let last_action = match tokio::time::timeout(TIMEOUT, self.last_action.lock()).await {
            Ok(guard) => guard.clone(),
            Err(_) => {
                eprintln!("makima: write_state: last_action lock timed out — possible deadlock");
                return;
            }
        };
        let held_keys = match tokio::time::timeout(TIMEOUT, self.held_keys.lock()).await {
            Ok(guard) => guard.clone(),
            Err(_) => {
                eprintln!("makima: write_state: held_keys lock timed out — possible deadlock");
                return;
            }
        };
        let base_name = self.config
            .iter()
            .find(|x| x.associations == Associations::default())
            .map(|x| x.name.clone())
            .unwrap_or_default();
        let config_stack = if config.name == base_name {
            vec![base_name.clone()]
        } else {
            let app_part = config.name
                .strip_prefix(&format!("{}::", base_name))
                .unwrap_or(&config.name)
                .to_string();
            vec![base_name, app_part]
        };
        crate::state_export::write_state(&config, &modifiers, layout, paused, &held_keys, &last_action, &config_stack).await;
    }

    /// Record the actual emitted key output for the HUD last-event display.
    /// Called at the emission site (not at input time) so the action reflects
    /// what was truly sent to the virtual device.
    async fn set_last_emitted(&self, _trigger: &Event, emitted: &[Key], value: i32, is_combo: bool, label: Option<String>) {
        if is_combo && value == 1 {
            let mut la = self.last_action.lock().await;
            *la = Some(LastAction {
                r#type: "keys".to_string(),
                value: serde_json::json!(
                    emitted.iter().map(|k| format!("{:?}", k)).collect::<Vec<_>>()
                ),
                ts: crate::state_export::now_ts(),
                label,
            });
        }
        self.write_state().await;
    }

    /// Record a spawned command as the last emitted action.
    async fn set_last_emitted_cmd(&self, _trigger: &Event, commands: &[String], label: Option<String>) {
        {
            let mut la = self.last_action.lock().await;
            *la = Some(LastAction {
                r#type: "command".to_string(),
                value: serde_json::Value::String(commands.join(" ")),
                ts: crate::state_export::now_ts(),
                label,
            });
        }
        self.write_state().await;
    }

    async fn start_control_socket(&self) {
        let paused         = self.paused.clone();
        let last_action    = self.last_action.clone();
        let current_config = self.current_config.clone();
        let modifiers      = self.modifiers.clone();
        let active_layout  = self.active_layout.clone();
        let held_keys      = self.held_keys.clone();
        let all_configs    = self.config.clone();
        tokio::spawn(async move {
            let _ = std::fs::remove_file("/tmp/makima-control.sock");
            let listener = match UnixListener::bind("/tmp/makima-control.sock") {
                Ok(listener) => listener,
                Err(e) => {
                    eprintln!("makima: control socket bind failed: {}", e);
                    return;
                }
            };
            while let Ok((stream, _addr)) = listener.accept().await {
                let paused         = paused.clone();
                let last_action    = last_action.clone();
                let current_config = current_config.clone();
                let modifiers      = modifiers.clone();
                let active_layout  = active_layout.clone();
                let held_keys      = held_keys.clone();
                let all_configs    = all_configs.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stream.into_std().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_ok() {
                        let cmd = line.trim();
                        match cmd {
                            "pause" | "resume" => {
                                let is_paused = cmd == "pause";
                                *paused.lock().await = is_paused;
                                // Write state immediately so the HUD sees the updated
                                // paused flag without waiting for the next button event.
                                const TIMEOUT: std::time::Duration =
                                    std::time::Duration::from_millis(200);
                                let config = match tokio::time::timeout(
                                    TIMEOUT, current_config.lock()).await {
                                    Ok(g) => g.clone(), Err(_) => return,
                                };
                                let mods = match tokio::time::timeout(
                                    TIMEOUT, modifiers.lock()).await {
                                    Ok(g) => g.clone(), Err(_) => return,
                                };
                                let layout = match tokio::time::timeout(
                                    TIMEOUT, active_layout.lock()).await {
                                    Ok(g) => *g, Err(_) => return,
                                };
                                let la = match tokio::time::timeout(
                                    TIMEOUT, last_action.lock()).await {
                                    Ok(g) => g.clone(), Err(_) => return,
                                };
                                let hk = match tokio::time::timeout(
                                    TIMEOUT, held_keys.lock()).await {
                                    Ok(g) => g.clone(), Err(_) => return,
                                };
                                let base_name = all_configs
                                    .iter()
                                    .find(|x| x.associations == Associations::default())
                                    .map(|x| x.name.clone())
                                    .unwrap_or_default();
                                let stack = if config.name == base_name {
                                    vec![base_name.clone()]
                                } else {
                                    let app_part = config.name
                                        .strip_prefix(&format!("{}::", base_name))
                                        .unwrap_or(&config.name)
                                        .to_string();
                                    vec![base_name, app_part]
                                };
                                crate::state_export::write_state(
                                    &config, &mods, layout, is_paused, &hk, &la, &stack,
                                ).await;
                            }
                            _ => {}
                        }
                    }
                });
            }
        });
    }

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
                            if !*self.paused.lock().await || self.settings.cursor_when_paused {
                                let (x_coord, y_coord) = if self.settings.invert_cursor_axis {
                                    (-stick_position[0], -stick_position[1])
                                } else {
                                    (stick_position[0], stick_position[1])
                                };
                                let virtual_event_x: InputEvent =
                                    InputEvent::new_now(EventType::RELATIVE, 0, x_coord);
                                let virtual_event_y: InputEvent =
                                    InputEvent::new_now(EventType::RELATIVE, 1, y_coord);
                                let mut virt_dev = self.virt_dev.lock().await;
                                virt_dev.axis.emit(&[virtual_event_x]).unwrap();
                                virt_dev.axis.emit(&[virtual_event_y]).unwrap();
                            }
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
                            if !*self.paused.lock().await || self.settings.cursor_when_paused {
                                let (x_coord, y_coord) = if self.settings.invert_scroll_axis {
                                    (-stick_position[0], -stick_position[1])
                                } else {
                                    (stick_position[0], stick_position[1])
                                };
                                let virtual_event_x: InputEvent =
                                    InputEvent::new_now(EventType::RELATIVE, 12, x_coord);
                                let virtual_event_y: InputEvent =
                                    InputEvent::new_now(EventType::RELATIVE, 11, y_coord);
                                let mut virt_dev = self.virt_dev.lock().await;
                                virt_dev.axis.emit(&[virtual_event_x]).unwrap();
                                virt_dev.axis.emit(&[virtual_event_y]).unwrap();
                            }
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
                if *cursor_movement == (0, 0) {
                    current_speed = 0.0
                } else {
                    current_speed += speed as f32 * acceleration / 10.0;
                    if current_speed > speed as f32 {
                        current_speed = speed as f32
                    }
                    if !*self.paused.lock().await || self.settings.cursor_when_paused {
                        if cursor_movement.0 != 0 {
                            let mut virt_dev = self.virt_dev.lock().await;
                            let virtual_event_x: InputEvent = InputEvent::new_now(
                                EventType::RELATIVE,
                                0,
                                cursor_movement.0 * current_speed as i32,
                            );
                            virt_dev.axis.emit(&[virtual_event_x]).unwrap();
                        }
                        if cursor_movement.1 != 0 {
                            let mut virt_dev = self.virt_dev.lock().await;
                            let virtual_event_y: InputEvent = InputEvent::new_now(
                                EventType::RELATIVE,
                                1,
                                cursor_movement.1 * current_speed as i32,
                            );
                            virt_dev.axis.emit(&[virtual_event_y]).unwrap();
                        }
                    }
                }
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
                if *scroll_movement == (0, 0) {
                    current_speed = 0.0
                } else {
                    current_speed += speed as f32 * acceleration / 10.0;
                    if current_speed > speed as f32 {
                        current_speed = speed as f32
                    }
                    if !*self.paused.lock().await || self.settings.cursor_when_paused {
                        let mut virt_dev = self.virt_dev.lock().await;
                        if scroll_movement.0 != 0 {
                            let virtual_event_x: InputEvent = InputEvent::new_now(
                                EventType::RELATIVE,
                                12,
                                scroll_movement.0 * current_speed as i32,
                            );
                            virt_dev.axis.emit(&[virtual_event_x]).unwrap();
                        }
                        if scroll_movement.1 != 0 {
                            let virtual_event_y: InputEvent = InputEvent::new_now(
                                EventType::RELATIVE,
                                11,
                                scroll_movement.1 * current_speed as i32,
                            );
                            virt_dev.axis.emit(&[virtual_event_y]).unwrap();
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}
