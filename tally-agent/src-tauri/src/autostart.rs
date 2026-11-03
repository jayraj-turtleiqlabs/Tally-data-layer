//! Windows Startup / Autostart Management.
//! Registers or unregisters the agent in HKCU\Software\Microsoft\Windows\CurrentVersion\Run.

pub const RUN_KEY_NAME: &str = "FinInsightTallyAgent";

#[cfg(target_os = "windows")]
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
#[cfg(target_os = "windows")]
use winreg::RegKey;

#[cfg(target_os = "windows")]
pub fn is_autostart_registered() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(run_key) = hkcu.open_subkey_with_flags(r"Software\Microsoft\Windows\CurrentVersion\Run", KEY_READ) {
        if let Ok(cmd) = run_key.get_value::<String, _>(RUN_KEY_NAME) {
            return !cmd.trim().is_empty();
        }
    }
    false
}

#[cfg(not(target_os = "windows"))]
pub fn is_autostart_registered() -> bool {
    false
}

#[cfg(target_os = "windows")]
pub fn set_autostart_registered(enable: bool) -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run_key, _) = hkcu
        .create_subkey_with_flags(r"Software\Microsoft\Windows\CurrentVersion\Run", KEY_WRITE)
        .map_err(|e| format!("Failed to access Windows Run registry key: {e}"))?;

    if enable {
        let exe_path = std::env::current_exe()
            .map_err(|e| format!("Failed to resolve current executable path: {e}"))?;
        let cmd = format!("\"{}\" --autostart", exe_path.to_string_lossy());
        run_key
            .set_value(RUN_KEY_NAME, &cmd)
            .map_err(|e| format!("Failed to set autostart registry entry: {e}"))?;
        log::info!("[autostart] Registered autostart on boot: {}", cmd);
    } else {
        let _ = run_key.delete_value(RUN_KEY_NAME);
        log::info!("[autostart] Unregistered autostart from registry");
    }

    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn set_autostart_registered(_enable: bool) -> Result<(), String> {
    Ok(())
}

/// Ensures the registry matches the user's saved setting.
pub fn sync_autostart_with_settings(enabled: bool) -> Result<(), String> {
    let currently_registered = is_autostart_registered();
    if currently_registered != enabled {
        set_autostart_registered(enabled)?;
    }
    Ok(())
}
