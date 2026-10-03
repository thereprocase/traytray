# Traytray

One tray icon per desktop, with an API behind it. Apps that support Traytray publish their
status, progress, messages and actions into it instead of adding their own tray icon. Tray
icons from other apps can be collected into a drawer.

**Status: pre-alpha. Nothing here is usable yet.** The design is in [docs/design.md](docs/design.md),
and every claim about runtime behavior is backed by an entry in [docs/testlog.md](docs/testlog.md).

- Platforms: Linux with KDE Plasma 6, and Windows 11.
- The host draws what apps send and passes clicks and replies back to them. It never runs
  commands for an app.
- Remote apps reach a host over a Tailscale tailnet after a one-time pairing.

## License

MIT. See [LICENSE](LICENSE).
