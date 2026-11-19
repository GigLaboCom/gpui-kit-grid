//! The examples' key tables. `secondary-` in a table is Cmd on macOS and
//! Ctrl elsewhere, as the application reports its host
//! (`App::platform_info`), except in a browser: there it stays Ctrl, on a
//! Mac too, because Cmd with a digit, N or Shift+R belongs to the browser
//! (its tabs, windows and reload). The grid's own keys follow the host
//! everywhere.
//!
//! `KeyBinding::new` and `Keystroke::parse` take explicit modifiers only,
//! so a table with `secondary-` binds through [`bind`] and shows its hints
//! through [`keystroke`].

use std::borrow::Cow;

use gpui_kit::{
    Action, App, KeyBinding, KeyBindingContextPredicate, Keystroke, PlatformInfo, PlatformTarget,
};

/// The binding of `keystrokes` to `action` in `context`, on this host.
pub fn bind(keystrokes: &str, action: impl Action, context: Option<&str>, cx: &App) -> KeyBinding {
    let predicate = context.map(|context| {
        KeyBindingContextPredicate::parse(context)
            .expect("valid key context")
            .into()
    });
    let keystrokes = resolve(keystrokes, cx.platform_info());
    KeyBinding::load(&keystrokes, Box::new(action), predicate, false, None, cx)
        .expect("valid keystroke")
}

/// The keystroke `keystrokes` stands for on `platform`, for a hint.
pub fn keystroke(keystrokes: &str, platform: PlatformInfo) -> Option<Keystroke> {
    Keystroke::parse_with_os(&resolve(keystrokes, platform), platform.os).ok()
}

/// `keystrokes` with `secondary-` spelled out as Ctrl in a browser.
fn resolve(keystrokes: &str, platform: PlatformInfo) -> Cow<'_, str> {
    if platform.target == PlatformTarget::Web {
        Cow::Owned(keystrokes.replace("secondary-", "ctrl-"))
    } else {
        Cow::Borrowed(keystrokes)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::OperatingSystem;

    use super::*;

    #[test]
    fn test_secondary_is_ctrl_in_a_browser() {
        let host = |os, target| PlatformInfo { os, target };
        let mac = host(OperatingSystem::Mac, PlatformTarget::Native);
        let mac_browser = host(OperatingSystem::Mac, PlatformTarget::Web);
        let linux = host(OperatingSystem::Linux, PlatformTarget::Native);
        for (platform, cmd) in [(mac, true), (mac_browser, false), (linux, false)] {
            let keystroke = keystroke("shift-secondary-1", platform).expect("valid keystroke");
            assert_eq!(keystroke.modifiers.platform, cmd, "{platform:?}");
            assert_eq!(keystroke.modifiers.control, !cmd, "{platform:?}");
            assert!(keystroke.modifiers.shift);
        }
    }
}
