//! Lua-scriptable, schedule-triggered automation for FocalDesk.
//!
//! [`config`] loads automation definitions (name, script, schedule) from
//! `~/.config/focaldesk/automation/automations.toml`. [`schedule`] parses
//! and evaluates the "every Ns/m/h" / "daily HH:MM" syntax. [`script`] runs
//! a `.lua` file in a fresh `mlua` VM with a small structured API
//! (`notify`, `volume`, `bluetooth_power`, `wifi_power`, `exec`, `log`)
//! built on top of the existing `focaldesk-ipc` clients rather than talking
//! to hardware directly. [`ipc`] exposes the running daemon's state
//! (`ListAutomations`/`RunNow`/`Status`) over its own Unix socket, mirroring
//! `focaldesk_ai::ipc`'s shape.

pub mod config;
pub mod ipc;
pub mod runner;
pub mod schedule;
pub mod script;
