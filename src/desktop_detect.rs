//! Which desktop is running, decided from the environment first and from the
//! process list only when the environment says nothing.
//!
//! makima runs as a systemd user unit. The unit used to hard-code
//! `XDG_CURRENT_DESKTOP=KDE`, and since `session::copy_variables` never
//! overrides a variable that is already set, every session — Hyprland
//! included — was taken for KDE. The unit now inherits the user manager's
//! environment, which the session imports before `graphical-session.target`
//! (Plasma's startplasma, uwsm on Hyprland/Omarchy). If a setup does not
//! import it, the running compositor process is the next best witness;
//! guessing a fixed default is what caused the bug in the first place.
//!
//! Like `session`, this module depends on nothing inside the crate.

/// Desktops that get per-application bindings, as `XDG_CURRENT_DESKTOP` names them.
pub const SUPPORTED: [&str; 4] = ["KDE", "Hyprland", "sway", "niri"];

/// Compositor process names (`/proc/<pid>/comm`) and the desktop each one means.
const COMPOSITOR_PROCESSES: [(&str, &str); 4] = [
    ("kwin_wayland", "KDE"),
    ("Hyprland", "Hyprland"),
    ("sway", "sway"),
    ("niri", "niri"),
];

/// Pick the desktop name makima works with.
///
/// * `xdg_current_desktop` — the variable as inherited, if set. It may be a
///   colon-separated list (`"ubuntu:GNOME"`); the first supported entry wins.
///   A set but unsupported value is returned as it is, so the caller can say
///   which desktop it does not support — the process list does not overrule it.
/// * `process_names` — `comm` of the running processes, consulted only when
///   the variable is unset or empty.
pub fn resolve_desktop<S: AsRef<str>>(
    xdg_current_desktop: Option<&str>,
    process_names: &[S],
) -> Option<String> {
    if let Some(value) = xdg_current_desktop.map(str::trim).filter(|v| !v.is_empty()) {
        let supported = value.split(':').find(|entry| SUPPORTED.contains(entry));
        return Some(supported.unwrap_or(value).to_string());
    }
    COMPOSITOR_PROCESSES
        .iter()
        .find(|(process, _)| process_names.iter().any(|name| name.as_ref() == *process))
        .map(|(_, desktop)| desktop.to_string())
}

/// `comm` of every process this user can see in `/proc`.
pub fn running_process_names() -> Vec<String> {
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.flatten()
        .filter(|entry| entry.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|entry| std::fs::read_to_string(entry.path().join("comm")).ok())
        .map(|comm| comm.trim_end().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: [&str; 0] = [];

    #[test]
    fn the_environment_wins_over_the_process_list() {
        assert_eq!(resolve_desktop(Some("Hyprland"), &["kwin_wayland"]).as_deref(), Some("Hyprland"));
        assert_eq!(resolve_desktop(Some("KDE"), &["Hyprland"]).as_deref(), Some("KDE"));
    }

    #[test]
    fn a_colon_separated_list_yields_its_first_supported_entry() {
        assert_eq!(resolve_desktop(Some("Hyprland:wlroots"), &NONE).as_deref(), Some("Hyprland"));
        assert_eq!(resolve_desktop(Some("foo:KDE"), &NONE).as_deref(), Some("KDE"));
    }

    #[test]
    fn an_unsupported_desktop_is_reported_not_replaced() {
        assert_eq!(resolve_desktop(Some("ubuntu:GNOME"), &["kwin_wayland"]).as_deref(), Some("ubuntu:GNOME"));
    }

    #[test]
    fn without_the_variable_the_running_compositor_decides() {
        assert_eq!(resolve_desktop(None, &["systemd", "kwin_wayland", "plasmashell"]).as_deref(), Some("KDE"));
        assert_eq!(resolve_desktop(None, &["systemd", "Hyprland", "waybar"]).as_deref(), Some("Hyprland"));
        assert_eq!(resolve_desktop(Some(""), &["Hyprland"]).as_deref(), Some("Hyprland"));
    }

    #[test]
    fn nothing_known_means_no_desktop() {
        assert_eq!(resolve_desktop(None, &["systemd", "bash"]), None);
        assert_eq!(resolve_desktop(None, &NONE), None);
    }

    #[test]
    fn the_resolved_name_selects_the_matching_adapter() {
        use crate::compositor::{detect, CompositorKind};
        let kind = |env, procs: &[&str]| resolve_desktop(env, procs).map(|d| detect(&d));
        assert_eq!(kind(Some("KDE"), &[]), Some(CompositorKind::Kde));
        assert_eq!(kind(Some("Hyprland"), &[]), Some(CompositorKind::Hyprland));
        assert_eq!(kind(None, &["Hyprland"]), Some(CompositorKind::Hyprland));
        assert_eq!(kind(None, &["kwin_wayland"]), Some(CompositorKind::Kde));
        assert_eq!(kind(Some("sway"), &[]), Some(CompositorKind::Fallback));
    }
}
