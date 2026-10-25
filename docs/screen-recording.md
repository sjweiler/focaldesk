# Native screen recording

`focaldesk-recorder` records one compositor output through FocalDesk's existing
bounded output-capture broker. It does not capture DRM framebuffers directly,
and it does not require OBS or an OBS portal source.

```sh
focaldesk-cli desktop-snapshot
focaldesk-recorder --output 1 --file "$HOME/Videos/demo.mp4" --confirm
```

The current initial release is video-only: SDR Rec.709/sRGB frames are encoded
as H.264 MP4 through FFmpeg. Audio, arbitrary window capture, and configurable
codecs are intentionally deferred. The compositor's capture boundary already
tone-maps HDR output to SDR, avoiding incorrect PQ or wide-gamut recordings on
ordinary players.

## Consent and AI control

Recording is an observable, explicit capability. The command requires
`--confirm`, refuses to overwrite an existing file, and only connects to the
same-session local capture socket. Future voice/AI intents must request native
recording consent and invoke this same recorder flow; they must not receive an
unrestricted framebuffer or silent-recording capability.
