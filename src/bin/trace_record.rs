// trace_record — standalone diagnostic tool.
//
// Records a raw, timestamped trace of the Steam Deck's gamepad evdev stream
// (ABS_HAT1X/Y, SYN_REPORT, gyro, ...) AND its hidraw touch-bit stream
// (byte 10, LPAD_TOUCH/RPAD_TOUCH) to stdout as JSON lines, with a shared
// monotonic clock so the two streams can be correlated afterwards.
//
// Purpose: capture reproducible "cursor jump on reposition" and "staircase
// diagonal" scenarios exactly as makima's event_reader.rs sees them, so they
// can be replayed deterministically in a test harness instead of relying on
// live hand-testing on the device.
//
// Usage:
//   cargo run --release --bin trace_record > trace.jsonl
//   (reproduce the glitch on the trackpad, then Ctrl-C)
//
// Output format (one JSON object per line):
//   {"t_us":1234,"src":"evdev","type":3,"code":19,"value":100}   // type/code = evdev EventType/code
//   {"t_us":1240,"src":"hidraw","byte10":24}

use evdev::Device;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

enum Line {
    Evdev { t_us: u128, ev_type: u16, code: u16, value: i32 },
    Hidraw { t_us: u128, byte10: u8, raw: [u8; 64] },
}

/// Duplicated from event_reader.rs::find_hidraw_for_evdev on purpose — this
/// tool must stay a standalone binary with no dependency on makima's
/// internals so it keeps working even while that code is being changed.
fn find_hidraw_for_evdev(evdev_path: &Path) -> Option<PathBuf> {
    let dev_name = evdev_path.file_name()?.to_str()?;
    let evdev_sysfs =
        std::fs::canonicalize(format!("/sys/class/input/{}/device", dev_name)).ok()?;
    // evdev_sysfs is …/usb_iface/HID_A/input/inputN — go up three levels.
    let usb_iface = evdev_sysfs.parent()?.parent()?.parent()?;
    for entry in std::fs::read_dir("/sys/class/hidraw/").ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Ok(hidraw_sysfs) =
            std::fs::canonicalize(format!("/sys/class/hidraw/{}/device", name))
        {
            if hidraw_sysfs.parent() == Some(usb_iface) {
                return Some(PathBuf::from(format!("/dev/{}", name)));
            }
        }
    }
    None
}

fn find_steam_deck_evdev() -> Option<PathBuf> {
    for (path, dev) in evdev::enumerate() {
        if dev.name() == Some("Steam Deck") {
            return Some(path);
        }
    }
    None
}

fn main() {
    let evdev_path = find_steam_deck_evdev().expect(
        "Steam Deck evdev device not found — is the controller connected and are you in the \
         `input` group?",
    );
    let hidraw_path = find_hidraw_for_evdev(&evdev_path);
    eprintln!("trace_record: evdev  = {:?}", evdev_path);
    eprintln!("trace_record: hidraw = {:?}", hidraw_path);
    if hidraw_path.is_none() {
        eprintln!(
            "trace_record: WARNING - no hidraw sibling found, trace will have NO touch-bit data \
             (jump repro needs this!)"
        );
    }
    eprintln!("trace_record: recording — reproduce the glitch now, then Ctrl-C. Writing JSON lines to stdout.");

    let (tx, rx) = mpsc::channel::<Line>();
    let start = Instant::now();

    // evdev reader thread — blocking fetch_events(), no tokio needed for a
    // one-shot recording tool.
    {
        let tx = tx.clone();
        let evdev_path = evdev_path.clone();
        std::thread::spawn(move || {
            let mut dev = Device::open(&evdev_path).expect("open evdev device");
            loop {
                match dev.fetch_events() {
                    Ok(events) => {
                        for ev in events {
                            let t_us = start.elapsed().as_micros();
                            let _ = tx.send(Line::Evdev {
                                t_us,
                                ev_type: ev.event_type().0,
                                code: ev.code(),
                                value: ev.value(),
                            });
                        }
                    }
                    Err(e) => {
                        eprintln!("trace_record: evdev read error: {e}");
                        break;
                    }
                }
            }
        });
    }

    // hidraw reader thread — mirrors run_hidraw_reader's framing (64-byte
    // fixed reports, touch bits in byte 10).
    if let Some(hidraw_path) = hidraw_path {
        let tx = tx.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut f = match std::fs::File::open(&hidraw_path) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("trace_record: cannot open hidraw device: {e}");
                    return;
                }
            };
            let mut buf = [0u8; 64];
            loop {
                match f.read_exact(&mut buf) {
                    Ok(_) => {
                        let t_us = start.elapsed().as_micros();
                        let _ = tx.send(Line::Hidraw { t_us, byte10: buf[10], raw: buf });
                    }
                    Err(_) => break,
                }
            }
        });
    }

    drop(tx);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in rx {
        let res = match line {
            Line::Evdev { t_us, ev_type, code, value } => writeln!(
                out,
                "{{\"t_us\":{t_us},\"src\":\"evdev\",\"type\":{ev_type},\"code\":{code},\"value\":{value}}}"
            ),
            Line::Hidraw { t_us, byte10, raw } => {
                let hex: String = raw.iter().map(|b| format!("{:02x}", b)).collect();
                writeln!(
                    out,
                    "{{\"t_us\":{t_us},\"src\":\"hidraw\",\"byte10\":{byte10},\"raw\":\"{hex}\"}}"
                )
            }
        };
        if res.is_err() {
            break;
        }
    }
}
