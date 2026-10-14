# Service configuration templates

<img width="360" alt="Okytes" src="../../../assets/images/brand/okytes_logo_transperant.png" />

No template is installed automatically. Edit paths and use the same account that
owns the profile and private token files. Avoid root/LocalSystem for a personal
download queue.

- **Linux:** place the unit in `~/.config/systemd/user/`, install the executable at
  its configured path, and use `systemctl --user daemon-reload` followed by
  `systemctl --user enable --now okytes-daemon`. User lingering is a separate host
  administration choice. Inspect shutdown/recovery before relying on unattended use.
- **macOS:** replace `YOUR_USER`, place the plist in `~/Library/LaunchAgents/`, and
  load it with `launchctl bootstrap gui/$(id -u) <absolute-plist-path>`. Paths in
  ProgramArguments are literal; launchd does not expand shell variables.
- **Windows:** the XML is for an external WinSW wrapper. A console executable
  cannot be registered directly with SCM. Configure the wrapper's service account
  as the profile owner and make the profile path explicit for that account.
  Wrapper installation/signing and account setup are separate, manual operations.

The templates use local-only API defaults. None contains a bearer token. Graceful
shutdown stops admission, drains synced checkpoints and leaves interrupted jobs
queued for startup. Supervisor tests on systemd, launchd and Windows SCM are
unverified until actually run on those platforms.

Reference contracts: [systemd service configuration](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html),
[Apple launchd jobs](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html)
and [WinSW configuration](https://github.com/winsw/winsw/blob/v3/docs/xml-config-file.md).
