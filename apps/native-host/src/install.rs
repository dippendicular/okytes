//! Per-user host registration. No elevation, system-wide writes or credential logs.
use clap::Parser;
use okytes_browser_bridge::{protocol::HOST_NAME, provision};
use serde_json::json;
use std::{io, path::PathBuf};

#[derive(Parser)]
struct Install {
    command: String,
    /// IDs of unpacked/store Chromium extensions (repeat for distinct browser IDs).
    #[arg(long)]
    chromium_id: Vec<String>,
    /// Firefox manifest browser_specific_settings.gecko.id.
    #[arg(long, default_value = "okytes@okytes.org")]
    firefox_id: String,
}

pub fn profile() -> io::Result<PathBuf> {
    if let Some(directory) = std::env::var_os("OKYTES_DATA_DIR") {
        let path = PathBuf::from(directory);
        if !path.is_absolute() {
            return Err(io::Error::other("profile must be absolute"));
        }
        return Ok(path);
    }
    #[cfg(windows)]
    let path = PathBuf::from(
        std::env::var_os("APPDATA").ok_or_else(|| io::Error::other("APPDATA unavailable"))?,
    )
    .join("org.okytes.desktop");
    #[cfg(target_os = "macos")]
    let path = PathBuf::from(
        std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME unavailable"))?,
    )
    .join("Library/Application Support/org.okytes.desktop");
    #[cfg(all(unix, not(target_os = "macos")))]
    let path = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or(
            PathBuf::from(
                std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME unavailable"))?,
            )
            .join(".local/share"),
        )
        .join("org.okytes.desktop");
    Ok(path)
}

pub fn caller_allowed(directory: &std::path::Path) -> bool {
    let Ok(bytes) = std::fs::read(directory.join("browser-hosts.json")) else {
        return false;
    };
    if bytes.len() > 4096 {
        return false;
    }
    let Ok(ids) = serde_json::from_slice::<Vec<String>>(&bytes) else {
        return false;
    };
    std::env::args().skip(1).any(|arg| ids.contains(&arg))
}

pub fn run() -> io::Result<()> {
    let input = Install::parse();
    if input.chromium_id.len() > 16
        || input
            .chromium_id
            .iter()
            .any(|id| id.len() != 32 || !id.bytes().all(|b| (b'a'..=b'p').contains(&b)))
        || input.firefox_id.len() > 128
        || !input.firefox_id.contains('@')
        || input.firefox_id.chars().any(char::is_control)
    {
        return Err(io::Error::other("invalid extension ID"));
    }
    let directory = profile()?;
    provision(&directory)?;
    let executable = std::env::current_exe()?;
    let origins: Vec<_> = input
        .chromium_id
        .iter()
        .map(|id| format!("chrome-extension://{id}/"))
        .collect();
    let mut callers = origins.clone();
    callers.push(input.firefox_id.clone());
    std::fs::write(
        directory.join("browser-hosts.json"),
        serde_json::to_vec(&callers)?,
    )?;
    let chromium = json!({"name":HOST_NAME,"description":"Okytes browser bridge","path":executable,"type":"stdio","allowed_origins":origins});
    let firefox = json!({"name":HOST_NAME,"description":"Okytes browser bridge","path":executable,"type":"stdio","allowed_extensions":[input.firefox_id]});
    #[cfg(windows)]
    {
        for (browser, manifest) in [
            ("Google\\Chrome", &chromium),
            ("Microsoft\\Edge", &chromium),
            ("Mozilla", &firefox),
        ] {
            let path = directory.join(format!("{}.json", browser.replace('\\', "-")));
            std::fs::write(&path, serde_json::to_vec_pretty(manifest)?)?;
            let status = std::process::Command::new("reg.exe")
                .args([
                    "add",
                    &format!("HKCU\\Software\\{browser}\\NativeMessagingHosts\\{HOST_NAME}"),
                    "/ve",
                    "/t",
                    "REG_SZ",
                    "/d",
                ])
                .arg(path)
                .arg("/f")
                .output()?;
            if !status.status.success() {
                return Err(io::Error::other("user host registry registration failed"));
            }
        }
    }
    #[cfg(unix)]
    {
        let base = PathBuf::from(
            std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME unavailable"))?,
        );
        #[cfg(target_os = "macos")]
        let locations = [
            (
                "Library/Application Support/Google/Chrome/NativeMessagingHosts",
                &chromium,
            ),
            (
                "Library/Application Support/Microsoft Edge/NativeMessagingHosts",
                &chromium,
            ),
            (
                "Library/Application Support/Mozilla/NativeMessagingHosts",
                &firefox,
            ),
        ];
        #[cfg(not(target_os = "macos"))]
        let locations = [
            (".config/google-chrome/NativeMessagingHosts", &chromium),
            (".config/chromium/NativeMessagingHosts", &chromium),
            (".mozilla/native-messaging-hosts", &firefox),
        ];
        for (location, manifest) in locations {
            let folder = base.join(location);
            std::fs::create_dir_all(&folder)?;
            std::fs::write(
                folder.join(format!("{HOST_NAME}.json")),
                serde_json::to_vec_pretty(manifest)?,
            )?;
        }
    }
    println!("Installed per-user Okytes browser host. Restart Okytes to enable the bridge.");
    Ok(())
}
