# Sanctum

An adaptive GTK4/libadwaita Home Assistant companion for Linux phones, tablets and desktops.

Status: development snapshot. The interface is the Home Assistant frontend; Sanctum adds the native companion features around it.

## What works

- Onboarding like the Android app: server address, with Home Assistant servers announced on the local network (Avahi) listed below it to pick from (HTTPS is tried first when the address has no scheme or says `http://`, and plain HTTP only if that fails), then the Home Assistant login page inside Sanctum (client ID `https://dunevent.net/sanctum`, PKCE; the page names Sanctum instead of that URL, and nothing reloads it while you type), then a device name with a suggested default (the phone model, or the host name on a PC), then a page showing the internal and external addresses found on the server (its configured URLs, its network announcement, and the sign-in address), editable, with a switch to add the current network to Home Networks (Skip leaves both alone; the page is left out when there is nothing to confirm), then a page offering Run in Background and Start at Login (both off by default) with what they enable, then a welcome page listing what can be set up under Preferences in Sanctum's menu. The system browser (loopback redirect) and a long-lived access token remain under the login page's menu
- Device registration with the `mobile_app` integration, using the chosen device name. Like the Android app's, the device ID stays the same across sign-outs and reinstalls (a keyed hash of `/etc/machine-id` and the user's UID), so Home Assistant replaces the earlier registration and keeps the device, its entity IDs and its notify action
- Settings that belong to a server (its addresses, home networks, device name, last page, accent color, Assist pipelines, sensor and location choices) are kept per server under `/net/dunevent/Sanctum/servers/<id>/`, apart from app-wide settings; settings from earlier versions move there on first start
- Credentials stored in the keyring (Secret portal inside Flatpak)
- WebSocket connection with automatic reconnect, OAuth token refresh, and a disconnected screen
- `sanctum --assist`: standalone text Assist window; input typed before the connection is ready is sent once connected
- Run in Background and Start at Login settings (Background portal)
- Self-signed certificates: trust prompt with SHA-256 pinning per host
- Internal and external URLs chosen by home network (NetworkManager): a home network can be a Wi-Fi name, a connection name such as "Home VPN", or an interface such as `wg0`
- The Home Assistant frontend is the interface: it fills the window in WebKitGTK, with its own sidebar, dashboards, settings and add-ons. It signs in through its external app protocol with Sanctum's own token, so there is no second login. Its Assist button opens Sanctum's Assist, Settings > Companion App opens Preferences, and its haptics play through feedbackd. A compact 32 px header in the toolbar's own colors adds Reload and Sanctum's menu (whose Default Dashboard item, Alt+Home, leaves any page or app panel for the dashboard Home Assistant opens by default, and is hidden while that dashboard shows), so the two read as one bar. The last page reopens with the window, and links to other sites open in the browser
- With the integrated titlebar, the frontend's toolbar acts as the titlebar for mouse and pen: dragging an empty part moves the window, and double, middle and right clicks follow the desktop's titlebar settings (right click opens the compositor's window menu, or a Minimize, Maximize and Close menu where the compositor has none)
- Integrated Titlebar (Experimental, Preferences, General) hides Sanctum's header and draws Sanctum's menu and the window buttons in a section of their own at the end of the frontend's toolbar, filled with the frontend's quiet primary fill (`--ha-color-fill-primary-normal-resting`: a pale tint of the theme's primary color in light mode, a deep shade in dark mode) and its matching foreground. Every entry of Sanctum's menu has an icon, drawn as custom rows since GTK menus show none. The toolbar's ⋮ menu moves into Sanctum's menu, under Default Dashboard, with the frontend's icons, and its button is hidden meanwhile; choosing an entry there acts as choosing it in the frontend, entries Sanctum's menu already has, such as Assist, are left out, and shortcut hints such as "(Ctrl + K)" show as the menu's own shortcut labels. Pages without a toolbar Sanctum can use, such as some app panels and custom panels, get Sanctum's compact header instead until a toolbar is back. The section is clear over the frontend's launch screen and keeps the toolbar's last height, so it doesn't jump when the page loads. Only the toolbar is padded to keep its buttons clear (its companion app inset variable, set on that one element and re-checked as pages change), so the page below keeps its full width
- Right clicking a page shows Copy, Paste and the other text entries plus Reload; pages that use right clicks themselves, such as code editors, keep their own menus. On touch screens, pulling down at the top of a page and holding briefly reloads it (Pull to Refresh in Preferences turns this off). Swipe Navigation, off by default, goes back or forward through the page history with a sideways swipe
- Sanctum's accent color follows the Home Assistant theme's primary color (kept between runs; Accent Color from Server in Preferences turns this off), and switching the system between light and dark switches the frontend too
- Zoom at the top of Sanctum's menu (− / percentage / +, 50% to 200%; Ctrl+plus, Ctrl+minus and Ctrl+0), remembered between runs and applied to the login page too
- Preferences, Performance: Drawing picks GPU (default), CPU Painting (tiles painted on CPU threads, composited on the GPU; applies after a restart) or No Acceleration (everything on the CPU, which some phone GPU drivers handle better; the frontend's view transitions are turned off then, because WebKit's web process aborts on them without acceleration). Reduce Motion tells pages to prefer reduced motion (through GTK's reduced-motion setting, which WebKit passes on), cuts the remaining CSS and scripted animations in the frontend to an instant change, and turns off view transitions, smooth scrolling and Sanctum's own animations. Show Frame Rate overlays the frames per second and the longest frame of the last half second, as the page's main thread sees them (it stays near the display's refresh rate while idle; stalls show up in the longest frame), and Rendering Information opens WebKit's `webkit://gpu` report. Keep Loaded in Background keeps the page alive while the window is closed, so it reopens instantly; Release After (10 minutes to 4 hours, or never; 30 minutes by default) frees it once the window has been closed that long. If the page's process ends anyway, the page shows a Retry button
- Detail sheets for notifications that point at an entity, with controls for lights (brightness, color temperature, color), fans, climate, covers, locks, alarm panels, media players, selects, numbers and switches, plus a 24 hour history graph
- Sensor agent: battery (UPower), network and Wi-Fi (NetworkManager), mobile signal and operator (ModemManager), screen, session lock, CPU, memory and boot time, each with its own switch under Preferences, Sensors
- Opt-in location through the location portal, with accuracy and interval settings and a fresh position on network changes
- Notifications from Home Assistant over the WebSocket push channel: title, message, tag replacement, `clear_notification`, priority, images, click targets (`entityId:` opens the entity's details, paths and links to the server open in the frontend, other URLs in the browser) and action buttons that fire `mobile_app_notification_action`
- FuriOS assistant button: when the button service is running, Preferences shows what each press does and can bind a short, double or long press to Assist, with a live test
- Voice Assist: a Voice toggle in the Assist window plays a soft two-note listening sound (Listening Sound under Preferences, Assist; synthesized by `build-aux/make-listening-tones.py`), streams the microphone to the assistant's speech-to-text, stops when Home Assistant hears the end of speech, and reads the reply aloud; "Use Voice Assistant" in Preferences starts listening whenever Assist opens, including from the assistant button. Turning the toggle off cancels the question without acting on it. For testing without a microphone, `SANCTUM_AUDIO_SOURCE` takes a GStreamer source description such as `audiotestsrc is-live=true`
- Device controls: automations send commands with `notify.mobile_app_<device>` (see below), and Volume Level, Volume Muted, Screen Brightness, Flashlight, Ringer Mode, Bluetooth, Bluetooth Connections, Media Session and Do Not Disturb sensors report the state. Preferences, Controls lists every command with its own switch, a copy button for an example action, and, for commands the sandbox blocks, the `flatpak override` or Flatseal step that allows them
- Confirmation before opening garage doors and gates, unlocking, and disarming

## Building

The development manifest targets the GNOME 50 runtime:

```sh
flatpak install flathub org.gnome.Sdk//50 org.gnome.Platform//50 org.freedesktop.Sdk.Extension.rust-stable//25.08
flatpak-builder --user --install --force-clean build build-aux/net.dunevent.Sanctum.json
flatpak run net.dunevent.Sanctum
```

GNOME Builder opens the project directly and uses the same manifest.

For day-to-day work, run `build-aux/dev.sh`. It asks for the architecture (this machine's, the other one, or both) and, when the result can be installed here, whether to write a Flatpak file, install it, or both. Files go to `dist/`, one per architecture, ready to copy to a phone and install with `flatpak install --user <file>`. Cargo's crates and compiled output are kept in `.flatpak/<arch>/`, so a change recompiles in seconds instead of rebuilding everything, and the dependency modules are rebuilt only when the manifest changes.

aarch64 builds on an x86_64 machine run the aarch64 compiler under QEMU user emulation (`qemu-user-static` registered with binfmt_misc). The first build compiles every crate under emulation and is slow; later builds only recompile Sanctum itself.

The development manifest lets Cargo download crates during the build. A Flathub release manifest needs offline sources generated with `flatpak-cargo-generator`.

Without Flatpak, Meson builds against the host libraries (GTK 4.18, libadwaita 1.7, libsoup 3.6, libsecret 0.21, blueprint-compiler):

```sh
meson setup _build -Dprofile=development
meson compile -C _build
meson test -C _build
```

## Command line

| Option | Effect |
| --- | --- |
| `--assist`, `-a` | Open only the Assist window |
| `--background`, `-b` | Start without a window (used by Start at Login) |

When Sanctum is already running, `gapplication action net.dunevent.Sanctum assist` opens Assist over D-Bus without starting a new process.

## Device commands

Send these as the `message` of a `notify.mobile_app_<device>` action, with any value in `data.command`. Names follow the Android app where it has an equivalent, but levels are percentages.

| Message | `data.command` | Effect |
| --- | --- | --- |
| `command_volume_level` | `0` to `100`, `mute`, `unmute` or `toggle_mute` | Output volume; setting a level also unmutes. Only `media_stream: music_stream` (the default) is accepted |
| `command_ringer_mode` | `normal`, `vibrate` or `silent` | feedbackd profile (full, quiet or silent) |
| `command_dnd` | `off`, `priority_only`, `alarms_only` or `total_silence` | GNOME's notification banners setting. Phosh's Do Not Disturb switch follows it, and Phosh goes silent and later restores the earlier profile itself. Any value but `off` turns it on |
| `command_media` | `media_command`: `play`, `pause`, `play_pause`, `next`, `previous` or `stop`; optional `media_package_name` such as `mpv` | MPRIS player, the playing one when none is named |
| `command_screen_on` | | Wakes the screen |
| `command_screen_off` | | Turns the screen off without locking (Sanctum only) |
| `command_lock` | | Locks the session (Sanctum only) |
| `command_screen_brightness_level` | `0` to `100` | Backlight; `0` is the dimmest setting that still lights the panel |
| `command_screen_off_timeout` | Milliseconds, `0` for never | Blank-screen delay |
| `command_flashlight` | `turn_on`, `turn_off` or `toggle` | The torch LED |
| `command_bluetooth` | `turn_on`, `turn_off` or `toggle` | The first BlueZ adapter |
| `command_update_sensors` | | Reads and reports every sensor now |
| `command_stop_tts` | | Stops a spoken Assist reply |
| `command_webview` | `entityId:light.kitchen`, a dashboard path or a URL | Opens Sanctum at that entity, or the page in the browser |

Volume uses `pactl` over the PulseAudio socket; brightness and the flashlight go through logind's `SetBrightness`, the screen through the compositor's `PowerSaveMode`, ringer mode through feedbackd, media through MPRIS and Bluetooth through BlueZ, all allowed by the manifest. Do Not Disturb and the screen timeout write desktop settings through dconf, which the manifest does not allow; Preferences, Controls shows the command that does:

| Commands | Permission |
| --- | --- |
| Do Not Disturb, screen timeout | `--talk-name=ca.desrt.dconf` |
| Do Not Disturb sensor | `--filesystem=xdg-config/dconf:ro` |

dconf takes writes over D-Bus but has no read method; programs read its database file directly, so the sensor needs file access as well.

Sensors and commands blocked by a permission, such as after an override removes one, are listed in Preferences with the same help.

For example: `flatpak override --user --talk-name=ca.desrt.dconf net.dunevent.Sanctum`, then quit and reopen Sanctum. Android-only commands such as `command_launch_app`, intents and BLE transmitters are logged and ignored.

```yaml
action: notify.mobile_app_furiphone
data:
  message: command_flashlight
  data:
    command: toggle
```

## Permissions

Each permission in the Flatpak manifest and what uses it, for review:

| Permission | Used for |
| --- | --- |
| `--share=network` | Talking to Home Assistant |
| `--share=ipc`, `--socket=wayland`, `--socket=fallback-x11`, `--device=dri` | Display |
| `--socket=pulseaudio` | Voice Assist recording and replies; volume commands and sensors |
| `--system-talk-name=org.freedesktop.NetworkManager` | Network sensors and choosing the internal or external URL on home networks |
| `--system-talk-name=org.freedesktop.Avahi` | Finding Home Assistant servers on the local network during setup |
| `--system-talk-name=org.freedesktop.UPower` | Battery sensors |
| `--system-talk-name=org.freedesktop.ModemManager1` | Mobile network sensors |
| `--system-talk-name=org.freedesktop.login1` | Session lock sensor and command, brightness and flashlight through `SetBrightness` (the backlight is `backlight/*`, or an LED such as `leds/lcd-backlight` on phones with Android kernels) |
| `--talk-name=io.furios.Flashlightd` | Flashlight sensor and command on FuriOS, whose torch belongs to flashlightd |
| `--system-talk-name=org.bluez` | Bluetooth sensors and command |
| `--talk-name=org.gnome.Mutter.DisplayConfig` | Screen on sensor and screen commands (Mutter and Phosh both provide it) |
| `--talk-name=io.FuriOS.AssistantButton` | Detecting the FuriOS assistant button and test presses |
| `--talk-name=org.sigxcpu.Feedback` | Ringer mode sensor and command, dashboard haptics |
| `--talk-name=org.mpris.MediaPlayer2.*` | Media session sensor and media command |
| `--filesystem=xdg-config/assistant-button:create` | Binding Assist to the FuriOS assistant button |

Location, notifications, the keyring, background running and opening links use portals and need no static permission.

## Layout

| Path | Contents |
| --- | --- |
| `src/ha/` | All Home Assistant networking (libsoup); the rest of the app uses only this module's API |
| `src/model/` | Live entities as GObjects for detail sheets, plus how they are presented |
| `src/ui/` | Widgets; static layout lives in `data/resources/ui/*.blp`; `dashboard.rs` is the WebKit view of the frontend and `page.js` the script it adds for titlebar gestures and pull to refresh |
| `src/agent/` | Sensor agent and location tracker |
| `src/control/` | Device commands, their availability, and the sandbox permission check |
| `src/application.rs` | Command line, `--assist` fast path, background hold |
| `src/background.rs` | Background portal request |
| `build-aux/` | Flatpak manifest, dist vendoring script, icon list and update script |
| `data/icons/mdi/` | Bundled subset of Material Design Icons (Apache-2.0), generated from `build-aux/mdi-icons.txt` |

## Icons

Entity and area icons come from [Material Design Icons](https://pictogrammers.com/library/mdi/), the set Home Assistant uses. Only the icons listed in `build-aux/mdi-icons.txt` are bundled. To add one, append its name and regenerate from the npm package:

```sh
npm pack @mdi/svg && tar xzf mdi-svg-*.tgz
build-aux/update-mdi-icons.py package
```

An entity whose `mdi:` icon is not bundled shows its domain's default icon.

## License

GPL-3.0-or-later
