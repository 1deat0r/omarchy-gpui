# Notification service

The GPUI shell includes an optional implementation of the reference
`org.freedesktop.Notifications` session-bus service. It claims the well-known
name only when no existing notification daemon owns it; a running Quickshell
notification service is left untouched.

The current adapter covers the protocol boundary needed by Omarchy clients:

- `GetCapabilities` and `GetServerInformation`;
- `Notify` with replacement IDs, urgency, timeout, image, glyph, and
  `omarchy-exec-argv` hints;
- `CloseNotification` and the `NotificationClosed` signal; and
- DND persistence at `~/.local/state/omarchy/notifications.json`, including
  the reference `omarchy-action` and critical `notify-send` bypass rules.

Accepted notifications are written atomically under
`~/.local/state/omarchy/notifications/` and forwarded to the GPUI event
stream. DND-silenced non-ephemeral notifications are moved directly into the
matching `history/` directory. The existing shell IPC history commands read
the same files.

The adapter is an incremental parity slice, not a completion claim. Full toast
stacking/layout, D-Bus action-button dispatch, image copying, and the complete
notification lifecycle still need differential tests before the notifications
row or the overall parity contract can be marked complete.

## Isolated runtime check

The service was exercised in a private `dbus-run-session` with a temporary
`HOME`: `GetCapabilities` returned the advertised capabilities, `Notify`
returned an allocated ID, replacement reused that ID and rewrote one live
file, and `CloseNotification` moved the final entry into `history/`.
