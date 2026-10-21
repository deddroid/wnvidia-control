# Changelog

## 0.1.0 — 2026-10-21

First release.

- Monitoring (temperature, clocks, power, fans, load, VRAM, P-state, limiters) with charts;
  power draw from driver samples where the instant reading is unsupported.
- Power limit, core/memory offsets, clock limit (undervolt), target temperature.
- Fan control: automatic, fixed, curve editor with hysteresis and slow-down delay.
- Profiles in TOML, apply at boot, tray menu, CLI.
- Root service with group-based access (no password prompts), watchdog re-apply,
  fans back to automatic on stop.
- English and Italian.
