use std::fs;

use gtk::glib;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceInfo {
    pub name: String,
    pub manufacturer: String,
    pub model: String,
    pub os_name: String,
    pub os_version: String,
}

impl DeviceInfo {
    /// An empty `name` uses the host name, which installs registered before
    /// names could be chosen were registered with.
    pub fn detect(name: &str) -> Self {
        let (manufacturer, model) = hardware();
        let (os_name, os_version) = os_release();
        Self {
            name: if name.trim().is_empty() {
                glib::host_name().to_string()
            } else {
                name.trim().to_owned()
            },
            manufacturer,
            model,
            os_name,
            os_version,
        }
    }
}

/// The name offered during sign-in: the devicetree model on phones, which
/// reads better than a host name, and the host name elsewhere.
pub fn suggested_name() -> String {
    read_trimmed("/sys/firmware/devicetree/base/model")
        .unwrap_or_else(|| glib::host_name().to_string())
}

/// The registration ID, the same on every sign-in by this user on this system,
/// as Android's per-app device ID is. Home Assistant then replaces the earlier
/// registration on a new sign-in, keeping the device and its entity IDs. A
/// keyed hash keeps the machine ID itself private. A new random ID is used
/// when the machine ID cannot be read.
pub fn stable_id() -> String {
    let Some(machine) = read_trimmed("/etc/machine-id") else {
        return glib::uuid_string_random().replace('-', "");
    };
    let uid = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata("/proc/self")
            .map(|meta| meta.uid())
            .unwrap_or(0)
    };
    derive_id(&machine, uid)
}

fn derive_id(machine: &str, uid: u32) -> String {
    glib::compute_hmac_for_data(
        glib::ChecksumType::Sha256,
        machine.as_bytes(),
        format!("{}:{uid}", crate::config::BASE_ID).as_bytes(),
    )
    .chars()
    .take(32)
    .collect()
}

fn read_trimmed(path: &str) -> Option<String> {
    let value = fs::read_to_string(path).ok()?;
    let value = value.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    (!value.is_empty()).then(|| value.to_owned())
}

/// Phones expose a devicetree model ("FuriLabs FLX1s"); PCs expose DMI fields.
fn hardware() -> (String, String) {
    if let Some(model) = read_trimmed("/sys/firmware/devicetree/base/model") {
        return split_vendor(&model);
    }
    let vendor = read_trimmed("/sys/devices/virtual/dmi/id/sys_vendor");
    let product = read_trimmed("/sys/devices/virtual/dmi/id/product_name");
    match (vendor, product) {
        (Some(vendor), Some(product)) => (vendor, product),
        (Some(vendor), None) => (vendor, "Linux device".into()),
        (None, Some(product)) => ("Unknown".into(), product),
        (None, None) => ("Unknown".into(), "Linux device".into()),
    }
}

fn split_vendor(model: &str) -> (String, String) {
    match model.split_once(' ') {
        Some((vendor, rest)) if !rest.is_empty() => (vendor.to_owned(), model.to_owned()),
        _ => ("Unknown".into(), model.to_owned()),
    }
}

/// Inside Flatpak `/etc/os-release` describes the runtime, so the host copy is read first.
fn os_release() -> (String, String) {
    let contents = fs::read_to_string("/run/host/os-release")
        .or_else(|_| fs::read_to_string("/etc/os-release"))
        .unwrap_or_default();
    parse_os_release(&contents)
}

fn parse_os_release(contents: &str) -> (String, String) {
    let field = |key: &str| {
        contents.lines().find_map(|line| {
            let value = line.strip_prefix(key)?.strip_prefix('=')?;
            Some(value.trim_matches('"').to_owned())
        })
    };
    (
        field("NAME").unwrap_or_else(|| "Linux".into()),
        field("VERSION_ID")
            .or_else(|| field("BUILD_ID"))
            .unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_os_release() {
        let sample = "NAME=\"FuriOS\"\nVERSION_ID=\"2.0\"\nID=furios\n";
        assert_eq!(parse_os_release(sample), ("FuriOS".into(), "2.0".into()));
        assert_eq!(
            parse_os_release("NAME=Arch Linux\nBUILD_ID=rolling\n"),
            ("Arch Linux".into(), "rolling".into())
        );
    }

    #[test]
    fn derives_stable_ids() {
        let id = derive_id("0123456789abcdef0123456789abcdef", 1000);
        assert_eq!(id.len(), 32);
        assert_eq!(id, derive_id("0123456789abcdef0123456789abcdef", 1000));
        assert_ne!(id, derive_id("0123456789abcdef0123456789abcdef", 1001));
        assert_ne!(id, derive_id("fedcba9876543210fedcba9876543210", 1000));
    }

    #[test]
    fn splits_devicetree_model() {
        assert_eq!(
            split_vendor("FuriLabs FLX1s"),
            ("FuriLabs".into(), "FuriLabs FLX1s".into())
        );
        assert_eq!(
            split_vendor("Pinephone"),
            ("Unknown".into(), "Pinephone".into())
        );
    }
}
