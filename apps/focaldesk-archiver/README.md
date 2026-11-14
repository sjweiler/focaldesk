# Focaldesk Archiver

A local desktop archiver built with Tauri 2, Rust, and a dependency-free HTML/CSS/JavaScript interface.

## Run

Install Rust and the [Tauri system prerequisites](https://v2.tauri.app/start/prerequisites/), then:

```sh
cargo run
```

No Node installation, frontend build step, or Tauri CLI is required. To build an optimized executable, run `cargo build --release`.

## Use

- **Create archive:** add files or a folder, select ZIP, TAR, TAR.GZ, or GZ, choose an output filename, and create.
- **Extract archive:** select a ZIP, TAR, TAR.GZ, TGZ, or GZ file and a destination directory. Extraction creates a folder named after the archive.
- Native drag and drop accepts sources in either mode.
- GZ supports a single file; TAR.GZ supports multiple files and folders.

Existing archives and extraction folders are not overwritten. Symbolic links and special files are unsupported. Extraction rejects unsafe paths and limits total file contents to 10 GiB. Failed operations clean up temporary output. File permissions and executable bits are not restored during extraction. Password-protected and split archives are unsupported.

Archive work runs on a background thread. The interface shows operation status; cancellation and per-file progress are not implemented.

## Verify

```sh
cargo test
cargo fmt --check
node --check ui/app.js
```
