# FocalDesk Entertainment

Run from the workspace root on Linux.

```sh
cargo run -p focaldesk-entertainment
```

The app uses Tauri 2, WebKitGTK and `xdg-open`. For a frontend-only preview, serve the `ui/` directory with a local HTTP server.

Browse eight categories with five free-access providers each. Providers handle playback, accounts, library access and regional availability. The planners use a local catalog and search links to official channels, free episodes and lessons. Music plans supply playlist starters for YouTube, SoundCloud and Bandcamp. My List and Recently Opened are stored on the device. Recently Opened records provider launches.

The movie catalog draws on [Tubi](https://tubitv.com/category/movies), [NFB](https://www.nfb.ca/film/rip_a_remix_manifesto/) and [Blender Studio](https://studio.blender.org/films/). Documentary picks come from [NASA+](https://plus.nasa.gov/). Rotating titles and search results depend on provider availability.
