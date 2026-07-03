
# FocalDesk Architecture

FocalDesk is a Rust-based Wayland desktop environment and compositor for Linux. It combines a custom compositor, desktop shell, launcher service, GTK companion applications, rendering effects, XWayland compatibility, PipeWire capture, and planned AI-assisted desktop workflows.

## Goals

- Build a functional Linux desktop environment around a custom Wayland compositor
- Keep the compositor focused on display, input, surfaces, rendering, and session behavior
- Move process launching and future automation into separate services
- Support real-world applications, including GTK, X11/XWayland, Wine, browsers, OBS, and games
- Provide a modular foundation for future AI-assisted workflows
- Favor practical usability over a toy compositor demo

## High-Level Overview

```text
Applications
├── Wayland clients
├── XWayland clients
├── GTK applications
├── Wine / DXVK applications
└── Desktop utilities

        │

FocalDesk Compositor
├── Wayland protocol handling
├── Output and monitor management
├── Input handling
├── Window and surface management
├── Workspaces
├── Damage tracking
├── Rendering pipeline
├── Shell UI / overlays
├── Lock screen
└── IPC integration

        │

Platform / Backend Layer
├── DRM/KMS backend
├── Winit backend
├── OpenGL ES rendering
├── PipeWire capture support
├── XWayland support
└── Hardware cursor support

        │

Desktop Services and Applications
├── focal-launchd
├── focal-launch-shared
├── focaldesk-settings
├── focaldesk-files
└── future AI / agent services

