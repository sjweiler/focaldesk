# Location services

Focaldesk integrates with the standard XDG Location portal rather than
collecting or distributing coordinates itself.

## Request flow

1. An application creates an `org.freedesktop.portal.Location` session.
2. `xdg-desktop-portal` reads Focaldesk's
   `org.freedesktop.impl.portal.Lockdown` backend.
3. When `privacy.location_services` is off, Focaldesk reports
   `disable-location=true` and the portal rejects `CreateSession` and `Start`.
4. When the setting is on, the portal obtains the user's decision through its
   configured Access backend, stores sandboxed-app decisions in
   `xdg-permission-store`, and asks GeoClue for location data.
5. GeoClue selects the best available provider, such as modem GPS, network
   NMEA, Wi-Fi positioning, or IP positioning.

The Focaldesk backend watches `settings.json` and publishes a D-Bus
`PropertiesChanged` signal when the toggle changes. The setting defaults to
off, so a missing or unreadable settings file fails closed.

## Components and packaging

- `focaldesk-portal --backend` owns
  `org.freedesktop.impl.portal.desktop.focaldesk`.
- `focaldesk.portal` advertises the Lockdown implementation.
- `focaldesk-portals.conf` routes Lockdown to Focaldesk, ScreenCast and
  Screenshot to the wlroots backend, and remaining interfaces to the GTK
  backend.
- `focaldesk-portald.service` runs for the Focaldesk graphical session.
- Focaldesk publishes `XDG_CURRENT_DESKTOP=focaldesk:wlroots` so the routing
  file is selected while retaining wlroots compatibility.

The runtime requires `xdg-desktop-portal` built with location support, an
Access portal backend, and GeoClue. Fedora's packages provide these pieces.

## Security boundary and limitations

This toggle governs applications that use the XDG Location portal. A native
application that has direct permission to call GeoClue, a separate GPS daemon,
or a remote geolocation service is outside the portal boundary and cannot be
contained by a compositor preference.

The upstream portal checks Lockdown when a session is created or started.
Turning the toggle off rejects new sessions and sessions that have not started
yet. The upstream API does not provide a desktop backend operation for
terminating an already-started Location session. Closing those sessions
immediately would currently require restarting `xdg-desktop-portal`, which
would also disrupt unrelated portal sessions and is therefore not done
automatically.

Per-app location decisions are stored by `xdg-permission-store`. Focaldesk
Settings lists both allowed and denied location decisions under Saved App
Permissions. Revoking one deletes only that application's `location/location`
entry, so the portal asks the application again on its next location request.
