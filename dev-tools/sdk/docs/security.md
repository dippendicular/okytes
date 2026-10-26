# SDK security

Plain HTTP clients accept loopback endpoints only. Remote daemon connections require HTTPS with normal certificate validation. Load tokens from protected storage, rotate them, use the lowest sufficient scope, and never place them in URLs or source control.

Read-only credentials inspect state. Control credentials create and control transfers. Admin credentials manage settings, tokens, and peer trust. SDK wrappers cannot bypass server-side scope, Host, Origin, destination-root, TLS, SSH host-key, or LAN trust checks. Errors and events expose sanitized diagnostics.
