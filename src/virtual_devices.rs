use evdev::{
    uinput::{VirtualDevice, VirtualDeviceBuilder},
    AbsoluteAxisType, AbsInfo, Key, PropType, UinputAbsSetup,
};

pub struct VirtualDevices {
    pub keys: VirtualDevice,
    pub axis: VirtualDevice,
    pub abs: VirtualDevice,
    pub lpad: Option<VirtualDevice>,
    pub rpad: Option<VirtualDevice>,
}

fn build_trackpad_device(name: &str) -> VirtualDevice {
    // Linux kernel axis numbers (include/uapi/linux/input-event-codes.h):
    //   ABS_X = 0x00 = 0,  ABS_Y = 0x01 = 1  (single-touch compat layer)
    //   ABS_MT_SLOT        = 0x2f = 47
    //   ABS_MT_POSITION_X  = 0x35 = 53
    //   ABS_MT_POSITION_Y  = 0x36 = 54
    //   ABS_MT_TRACKING_ID = 0x39 = 57
    // Steam Deck trackpad raw range: signed 16-bit, -32767..32767.
    // Physical size ~50x50mm → resolution = 65534 / 50 ≈ 1311 units/mm.
    // libinput rejects devices with nonsensical dimensions (resolution=1 → 65m pad).
    let range = AbsInfo::new(0, -32767, 32767, 0, 0, 1311);
    let slot_info = AbsInfo::new(0, 0, 1, 0, 0, 0);    // 2 slots (0..=1)
    let id_info = AbsInfo::new(-1, -1, 65535, 0, 0, 0); // -1 = no touch

    let mut pad_keys = evdev::AttributeSet::new();
    pad_keys.insert(Key::BTN_TOUCH);       // 0x14a = 330 — finger contact
    pad_keys.insert(Key::BTN_TOOL_FINGER); // 0x145 = 325 — finger tool type
    pad_keys.insert(Key::BTN_LEFT);        // 0x110 = 272 — physical click

    // INPUT_PROP_POINTER (0): tells libinput this device controls a pointer.
    // INPUT_PROP_BUTTONPAD (2): tells libinput the click button is under the
    // touch surface (clickpad), matching the Steam Deck trackpad hardware.
    let mut props = evdev::AttributeSet::<PropType>::new();
    props.insert(PropType::POINTER);   // 0
    props.insert(PropType::BUTTONPAD); // 2

    VirtualDeviceBuilder::new()
        .expect("Unable to create virtual trackpad device")
        .name(name)
        .with_properties(&props).unwrap()
        .with_keys(&pad_keys).unwrap()
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisType::ABS_X, range)).unwrap()
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisType::ABS_Y, range)).unwrap()
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisType(47), slot_info)).unwrap()  // ABS_MT_SLOT
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisType(53), range)).unwrap()      // ABS_MT_POSITION_X
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisType(54), range)).unwrap()      // ABS_MT_POSITION_Y
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisType(57), id_info)).unwrap()    // ABS_MT_TRACKING_ID
        .build()
        .unwrap()
}

impl VirtualDevices {
    pub fn new(device: evdev::Device) -> Self {
        let mut key_capabilities = evdev::AttributeSet::new();
        for i in 1..334 {
            key_capabilities.insert(Key(i));
        }
        let mut axis_capabilities = evdev::AttributeSet::new();
        for i in 0..13 {
            axis_capabilities.insert(evdev::RelativeAxisType(i));
        }
        let mut tablet_abs_capabilities: Vec<evdev::UinputAbsSetup> = Vec::new();
        if let Ok(absinfo) = device.get_abs_state() {
            for (axis_type, info) in absinfo.iter().enumerate() {
                if [0, 1, 2, 5, 6, 8, 24, 25, 26, 27, 40].contains(&axis_type) {
                    let new_absinfo = evdev::AbsInfo::new(
                        info.value,
                        info.minimum,
                        info.maximum,
                        info.fuzz,
                        info.flat,
                        info.resolution,
                    );
                    tablet_abs_capabilities.push(evdev::UinputAbsSetup::new(
                        evdev::AbsoluteAxisType(axis_type.try_into().unwrap()),
                        new_absinfo,
                    ))
                }
            }
        }
        let mut tablet_capabilities = evdev::AttributeSet::new();
        for i in 272..277 {
            tablet_capabilities.insert(evdev::Key(i));
        }
        for i in 320..325 {
            tablet_capabilities.insert(evdev::Key(i));
        }
        for i in 326..328 {
            tablet_capabilities.insert(evdev::Key(i));
        }
        for i in 330..333 {
            tablet_capabilities.insert(evdev::Key(i));
        }
        let mut tab_rel = evdev::AttributeSet::new();
        tab_rel.insert(evdev::RelativeAxisType(8));
        let mut tab_msc = evdev::AttributeSet::new();
        tab_msc.insert(evdev::MiscType(0));
        let pointer_prop = device.properties();
        let keys_builder = VirtualDeviceBuilder::new()
            .expect("Unable to create virtual device through uinput. Take a look at the Troubleshooting section for more info.")
            .name("Makima Virtual Keyboard/Mouse")
            .with_keys(&key_capabilities).unwrap();
        let axis_builder = VirtualDeviceBuilder::new()
            .expect("Unable to create virtual device through uinput. Take a look at the Troubleshooting section for more info.")
            .name("Makima Virtual Pointer")
            .with_relative_axes(&axis_capabilities).unwrap();
        let mut abs_builder = VirtualDeviceBuilder::new()
            .expect("Unable to create virtual device through uinput. Take a look at the Troubleshooting section for more info.")
            .name("Makima Virtual Pen/Tablet")
            .with_properties(&pointer_prop).unwrap()
            .with_msc(&tab_msc).unwrap()
            .with_relative_axes(&tab_rel).unwrap()
            .with_keys(&tablet_capabilities).unwrap()
            .input_id(device.input_id());
        for abs_setup in tablet_abs_capabilities {
            abs_builder = abs_builder.with_absolute_axis(&abs_setup).unwrap();
        }
        let virtual_device_keys = keys_builder.build().unwrap();
        let virtual_device_axis = axis_builder.build().unwrap();
        let virtual_device_abs = abs_builder.build().unwrap();
        Self {
            keys: virtual_device_keys,
            axis: virtual_device_axis,
            abs: virtual_device_abs,
            lpad: None,
            rpad: None,
        }
    }

    /// Call after construction to enable the virtual MT trackpad devices.
    /// `lpad` / `rpad`: which of the two pads to activate (controlled by config).
    pub fn enable_trackpads(&mut self, lpad: bool, rpad: bool) {
        if lpad {
            self.lpad = Some(build_trackpad_device("Deckery Left Trackpad"));
        }
        if rpad {
            self.rpad = Some(build_trackpad_device("Deckery Right Trackpad"));
        }
    }
}
