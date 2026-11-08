use bitwarden_core::{DeviceType, HostPlatformInfo, init_host_platform_info};

/// Sent as `Bitwarden-Client-Version`. Servers use it as a feature gate, not as a client
/// identity: without it, Vaultwarden and the official server treat us as a pre-2024.12
/// client and drop SSH key items from `/sync`. We claim the lowest version that unlocks
/// what the SDK already handles, and raise it only when another gate is hit
/// (phase1.md Q1).
const FEATURE_LEVEL: &str = "2024.12.0";

/// What the server shows nibwarden as, in its sign-in emails and device list: the desktop
/// app of the platform. Sent with every request and as the login's `deviceType`.
pub(crate) const DEVICE_TYPE: DeviceType = if cfg!(target_os = "windows") {
    DeviceType::WindowsDesktop
} else if cfg!(target_os = "macos") {
    DeviceType::MacOsDesktop
} else {
    DeviceType::LinuxDesktop
};

/// Registers how nibwarden identifies itself to the server. The SDK keeps this in a
/// process-wide `OnceLock`, so only the first call takes effect.
///
/// `Bitwarden-Client-Name` is derived by the SDK from `device_type` and cannot be set
/// freely; the user agent is the only place nibwarden can name itself.
pub(crate) fn init(device_identifier: &str) {
    init_host_platform_info(HostPlatformInfo {
        user_agent: format!("nibwarden/{}", env!("CARGO_PKG_VERSION")),
        device_type: DEVICE_TYPE,
        device_identifier: Some(device_identifier.to_owned()),
        bitwarden_client_version: Some(FEATURE_LEVEL.to_owned()),
        bitwarden_package_type: None,
    });
}
