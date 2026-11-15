#!/usr/bin/env python3
"""Small, input-transparent GTK animation on one output; reports frame-clock pacing."""
import argparse
import json
import time
import os
import sys

# Python GI otherwise loads Wayland before layer-shell can install its hooks.
if "libgtk4-layer-shell.so" not in os.environ.get("LD_PRELOAD", ""):
    env = os.environ.copy()
    env["LD_PRELOAD"] = "libgtk4-layer-shell.so.0 " + env.get("LD_PRELOAD", "")
    os.execve(sys.executable, [sys.executable, *sys.argv], env)

import gi
gi.require_version('Gtk', '4.0')
gi.require_version('Gdk', '4.0')
gi.require_version('Gtk4LayerShell', '1.0')
from gi.repository import Gtk, Gdk, GLib, Gtk4LayerShell as LayerShell
import cairo

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--connector', required=True)
parser.add_argument('--seconds', type=float, default=30)
parser.add_argument('--output', required=True)
args = parser.parse_args()
if args.seconds <= 0:
    parser.error("seconds must be positive")
app = Gtk.Application(application_id='dev.focaldesk.IntelFrameProbe')
timestamps = []
started = None
monitor_name = None

def activate(app):
    global started, monitor_name
    display = Gdk.Display.get_default()
    monitors = display.get_monitors()
    matches = [monitors.get_item(i) for i in range(monitors.get_n_items())
               if monitors.get_item(i).get_connector() == args.connector]
    if len(matches) != 1:
        raise RuntimeError(f'Expected one monitor for {args.connector}, found {len(matches)}')
    monitor_name = matches[0].get_connector()
    window = Gtk.ApplicationWindow(application=app)
    LayerShell.init_for_window(window)
    if not LayerShell.is_layer_window(window):
        raise RuntimeError('Could not create a layer-shell probe; refusing off-output measurements')
    LayerShell.set_namespace(window, 'focaldesk-inference-frame-probe')
    LayerShell.set_monitor(window, matches[0])
    LayerShell.set_layer(window, LayerShell.Layer.OVERLAY)
    LayerShell.set_anchor(window, LayerShell.Edge.TOP, True)
    LayerShell.set_anchor(window, LayerShell.Edge.RIGHT, True)
    LayerShell.set_exclusive_zone(window, 0)
    LayerShell.set_keyboard_mode(window, LayerShell.KeyboardMode.NONE)
    window.set_default_size(24, 24)
    area = Gtk.DrawingArea()
    window.set_child(area)
    def draw(area, context, width, height):
        value = (time.monotonic() * 2) % 1
        context.set_source_rgb(value, 0.5, 1 - value)
        context.paint()
    area.set_draw_func(draw)
    def tick(widget, clock):
        timestamps.append(time.monotonic_ns())
        widget.queue_draw()
        return GLib.SOURCE_CONTINUE
    area.add_tick_callback(tick)
    window.connect('realize', lambda w: w.get_surface().set_input_region(cairo.Region()))
    started = time.monotonic()
    window.present()
    GLib.timeout_add(int(args.seconds * 1000), lambda: (app.quit(), False)[1])

app.connect('activate', activate)
app.run([])
# Frame callbacks measure scheduling, not physical presentation or touch latency.
intervals = [(b-a)/1e6 for a,b in zip(timestamps, timestamps[1:])][5:]
if not intervals:
    raise SystemExit('No frame samples; probe did not render on the requested output')
ordered = sorted(intervals)
def percentile(q):
    return ordered[min(len(ordered)-1, round((len(ordered)-1)*q))]
result = dict(connector=monitor_name, samples=len(intervals),
              elapsed_seconds=time.monotonic()-started,
              median_ms=percentile(.5), p95_ms=percentile(.95),
              p99_ms=percentile(.99), max_ms=max(intervals),
              over_33ms=sum(v>33.34 for v in intervals),
              measurement='GTK frame callback intervals; not touch or physical presentation latency')
with open(args.output, 'w') as handle:
    json.dump(result, handle, indent=2)
print(json.dumps(result), flush=True)
