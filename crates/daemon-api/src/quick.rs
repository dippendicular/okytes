//! Explicit native browser launch; credentials are never accepted in the URL.
use okytes_core::error::{EngineError, EngineResult};

/// Open only an IP-loopback Quick page in the user's default browser.
pub fn open_quick(value: &str) -> EngineResult<()> {
    let invalid = || EngineError::Config("cannot open local Quick Download page".into());
    let url = reqwest::Url::parse(value).map_err(|_| invalid())?;
    if url.scheme() != "http"
        || !url.host_str().is_some_and(|host| {
            host.parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        })
        || url.path() != "/quick"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid());
    }
    #[cfg(target_os = "windows")]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let mut command = std::process::Command::new("rundll32.exe");
        command
            .args(["url.dll,FileProtocolHandler", value])
            .creation_flags(0x08000000);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(value);
        command
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(value);
        command
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|_| invalid())?
        .success()
        .then_some(())
        .ok_or_else(invalid)?;
    Ok(())
}
