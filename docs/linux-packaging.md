# Linux Packaging Notes

Neko Route uses Tauri 2 and the platform webview. Linux packaging needs the same system packages normally required by Tauri plus a Secret Service provider for the key vault.

Required runtime/build dependencies vary by distribution, but include:

- WebKitGTK 4.1 development libraries
- GTK 3 development libraries
- libayatana-appindicator development libraries for tray integration
- Secret Service provider such as GNOME Keyring or KWallet
- pkg-config, glib, libsoup, and standard build tools

Neko Route does not fall back to plaintext API key storage when Secret Service is unavailable.
