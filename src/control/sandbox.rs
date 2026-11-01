//! Permissions are read from `/.flatpak-info`, which includes user overrides
//! once Sanctum restarts.

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    SessionTalk(&'static str),
    SystemTalk(&'static str),
    Socket(&'static str),
    /// A `--filesystem` value such as `xdg-config/dconf:ro`.
    Filesystem(&'static str),
}

impl Permission {
    pub fn flag(self) -> String {
        match self {
            Self::SessionTalk(name) => format!("--talk-name={name}"),
            Self::SystemTalk(name) => format!("--system-talk-name={name}"),
            Self::Socket(socket) => format!("--socket={socket}"),
            Self::Filesystem(path) => format!("--filesystem={path}"),
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Policy {
    pub sandboxed: bool,
    session: Vec<String>,
    system: Vec<String>,
    sockets: Vec<String>,
    filesystems: Vec<String>,
}

impl Policy {
    pub fn current() -> &'static Policy {
        static POLICY: OnceLock<Policy> = OnceLock::new();
        POLICY.get_or_init(|| match std::fs::read_to_string("/.flatpak-info") {
            Ok(info) => Self::parse(&info),
            Err(_) => Self::default(),
        })
    }

    pub fn parse(info: &str) -> Self {
        let mut policy = Self {
            sandboxed: true,
            ..Self::default()
        };
        let mut section = "";
        for line in info.lines().map(str::trim) {
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name;
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            // `see` permits no method calls.
            let talks = matches!(value, "talk" | "own");
            match section {
                "Session Bus Policy" if talks => policy.session.push(key.to_owned()),
                "System Bus Policy" if talks => policy.system.push(key.to_owned()),
                "Context" if key == "sockets" => policy.sockets = granted(value),
                "Context" if key == "filesystems" => policy.filesystems = granted(value),
                _ => {}
            }
        }
        policy
    }

    pub fn allows(&self, permission: Permission) -> bool {
        if !self.sandboxed {
            return true;
        }
        let has_socket = |socket: &str| self.sockets.iter().any(|s| s == socket);
        match permission {
            Permission::SessionTalk(name) => {
                has_socket("session-bus") || self.session.iter().any(|rule| covers(rule, name))
            }
            Permission::SystemTalk(name) => {
                has_socket("system-bus") || self.system.iter().any(|rule| covers(rule, name))
            }
            Permission::Socket(socket) => has_socket(socket),
            Permission::Filesystem(wanted) => {
                let wanted = wanted.split(':').next().unwrap_or(wanted);
                self.filesystems.iter().any(|entry| {
                    let path = entry.split(':').next().unwrap_or(entry);
                    // A parent such as `home` or `xdg-config` also grants it.
                    matches!(path, "host" | "home")
                        || wanted == path
                        || wanted
                            .strip_prefix(path)
                            .is_some_and(|rest| rest.starts_with('/'))
                })
            }
        }
    }
}

/// Entries of a `;`-separated list, without the ones a `!` revokes.
fn granted(value: &str) -> Vec<String> {
    value
        .split(';')
        .filter(|entry| !entry.is_empty() && !entry.starts_with('!'))
        .map(str::to_owned)
        .collect()
}

/// A `.*` rule also covers a wanted name that is the same wildcard.
fn covers(rule: &str, wanted: &str) -> bool {
    if rule == wanted {
        return true;
    }
    match rule.strip_suffix(".*") {
        Some(prefix) => wanted
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.')),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = "[Application]\nname=net.dunevent.Sanctum\n\n[Context]\nshared=network;ipc;\nsockets=wayland;pulseaudio;!x11;\nfilesystems=xdg-config/assistant-button:create;xdg-config/dconf:ro;\n\n[Session Bus Policy]\norg.gnome.Mutter.DisplayConfig=talk\norg.mpris.MediaPlayer2.*=talk\norg.example.Seen=see\n\n[System Bus Policy]\norg.freedesktop.login1=talk\n";

    #[test]
    fn reads_policy() {
        let policy = Policy::parse(INFO);
        assert!(policy.allows(Permission::Socket("pulseaudio")));
        assert!(!policy.allows(Permission::Socket("x11")));
        assert!(policy.allows(Permission::SessionTalk("org.gnome.Mutter.DisplayConfig")));
        assert!(policy.allows(Permission::SessionTalk("org.mpris.MediaPlayer2.*")));
        assert!(policy.allows(Permission::SessionTalk("org.mpris.MediaPlayer2.mpv")));
        assert!(!policy.allows(Permission::SessionTalk("org.example.Seen")));
        assert!(!policy.allows(Permission::SessionTalk("org.sigxcpu.Feedback")));
        assert!(policy.allows(Permission::SystemTalk("org.freedesktop.login1")));
        assert!(!policy.allows(Permission::SystemTalk("org.bluez")));
        assert!(policy.allows(Permission::Filesystem("xdg-config/dconf:ro")));
        assert!(!policy.allows(Permission::Filesystem("xdg-data/foo:ro")));
        assert!(Policy::default().allows(Permission::SystemTalk("org.bluez")));
    }
}
