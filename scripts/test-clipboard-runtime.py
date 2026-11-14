"""Regression: Writer copy after a WebKit window disappears must replace old text.

Build first: cargo build -p focaldesk-desktop --no-default-features --features winit,xwayland
Run in a Wayland session with /usr/bin/python3, PyGObject, GTK 3, WebKit 4.1,
LibreOffice and python3-uno available. Uses a private nested compositor, Writer
profile, document and clipboard. Artifacts are retained under /tmp.
"""
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parents[1]
OLD = 'stale clipboard regression marker'
NEW = 'fresh clipboard regression marker'


def wait(predicate, label, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(.1)
    raise AssertionError('Timed out: ' + label)


def writer(root):
    import uno
    local = uno.getComponentContext()
    resolver = local.ServiceManager.createInstanceWithContext(
        'com.sun.star.bridge.UnoUrlResolver', local)

    def connect():
        try:
            return resolver.resolve('uno:pipe,name=' + os.environ['CLIPBOARD_TEST_PIPE']
                                    + ';urp;StarOffice.ComponentContext')
        except Exception:
            return None

    context = wait(connect, 'Writer UNO connection')
    manager = context.ServiceManager
    desktop = manager.createInstanceWithContext('com.sun.star.frame.Desktop', context)
    document = desktop.loadComponentFromURL('private:factory/swriter', '_blank', 0, ())
    dispatcher = manager.createInstanceWithContext('com.sun.star.frame.DispatchHelper', context)

    def copy(text):
        document.Text.setString(text)
        for command in ['.uno:SelectAll', '.uno:Copy']:
            dispatcher.executeDispatch(document.CurrentController.Frame, command, '', 0, ())
        time.sleep(1)

    time.sleep(2)
    copy(OLD)
    (root / 'copied').touch()
    wait(lambda: (root / 'recopy').exists(), 'WebKit window hidden')
    time.sleep(1)
    copy(NEW)
    (root / 'fresh').touch()
    # Keep the clipboard owner alive until the second paste has completed.
    wait(lambda: (root / 'result').exists(), 'second paste')


def webkit(root):
    import gi
    gi.require_version('Gtk', '3.0')
    gi.require_version('WebKit2', '4.1')
    from gi.repository import Gtk, GLib, WebKit2
    window = Gtk.Window(title='Clipboard regression — isolated WebKit input')
    view = WebKit2.WebView()
    window.add(view)
    view.load_html('<html><body><input id="paste" autofocus></body></html>', None)
    window.show_all()
    view.grab_focus()
    stage = [0]

    def paste():
        view.execute_editing_command('Paste')
        GLib.timeout_add(1000, read_result)
        return False

    def read_result():
        def result(webview, result, _data=None):
            value = webview.evaluate_javascript_finish(result).to_string()
            if stage[0] == 0:
                (root / 'initial').write_text(value)
                stage[0] = 1
                # Hiding destroys the active native surface. The compositor must
                # return keyboard AND selection focus to the revealed Writer.
                window.hide()
                (root / 'recopy').touch()
                GLib.timeout_add(100, fresh)
            else:
                (root / 'result').write_text(value)
                Gtk.main_quit()
        view.evaluate_javascript('document.getElementById("paste").value',
                                 -1, None, None, None, result, None)
        return False

    def fresh():
        if not (root / 'fresh').exists():
            return True
        view.evaluate_javascript('document.getElementById("paste").value=""',
                                 -1, None, None, None, None, None)
        window.show()
        view.grab_focus()
        GLib.timeout_add(2000, paste)
        return False

    GLib.timeout_add(2000, paste)
    Gtk.main()


def main():
    root = Path(tempfile.mkdtemp(prefix='focaldesk-clipboard-runtime-'))
    print('Artifacts:', root, flush=True)
    env = os.environ.copy()
    display = env.get('WAYLAND_DISPLAY', 'focaldesk-0')
    if not os.path.isabs(display):
        display = str(Path(env.get('XDG_RUNTIME_DIR', f'/run/user/{os.getuid()}')) / display)
    assert Path(display).is_socket(), 'A host Wayland session is required'
    for kind in ['config', 'data', 'cache', 'state', 'runtime']:
        (root / kind).mkdir(mode=0o700)
        env['XDG_' + ('RUNTIME_DIR' if kind == 'runtime' else kind.upper() + '_HOME')] = str(root / kind)
    env.update(WAYLAND_DISPLAY=display, FOCALDESK_DISABLE_PORTAL_ENV='1',
               FOCALDESK_DESKTOP_SOCKET_PATH=str(root / 'desktop.sock'),
               FOCALDESK_LOG_FILE=str(root / 'compositor.log'),
               LIBGL_ALWAYS_SOFTWARE='1', GDK_BACKEND='wayland',
               CLIPBOARD_TEST_PIPE=root.name.replace('-', '_'),
               RUST_LOG='warn,smithay::wayland::selection=debug')
    children = []
    with open(root / 'stderr', 'w') as log:
        def launch(args):
            process = subprocess.Popen(args, env=env, stdout=log, stderr=log,
                                       cwd=REPO, start_new_session=True)
            children.append(process)
            return process
        try:
            compositor = launch([str(REPO / 'target/debug/focaldesk-desktop')])
            def socket_ready():
                assert compositor.poll() is None, 'Nested compositor exited; inspect stderr'
                return next((p for p in (root / 'runtime').glob('focaldesk-*') if p.is_socket()), None)
            env['WAYLAND_DISPLAY'] = str(wait(socket_ready, 'nested Wayland socket'))
            launch(['libreoffice', '-env:UserInstallation=' + (root / 'writer-profile').as_uri(),
                    '--norestore', '--nodefault', '--accept=pipe,name=' + env['CLIPBOARD_TEST_PIPE']
                    + ';urp;StarOffice.ServiceManager'])
            launch(['/usr/bin/python3', __file__, 'writer', str(root)])
            wait(lambda: (root / 'copied').exists(), 'initial Writer copy')
            launch(['/usr/bin/python3', __file__, 'webkit', str(root)])
            wait(lambda: (root / 'result').exists(), 'second WebKit paste')
            assert (root / 'initial').read_text() == OLD, 'Initial clipboard setup failed'
            assert (root / 'result').read_text() == NEW, 'New Writer copy was rejected or stale text pasted'
            print('PASS: Writer replaces old clipboard data after active WebKit surface destruction')
        finally:
            for process in reversed(children):
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait()


if __name__ == '__main__':
    if len(sys.argv) == 3:
        {'writer': writer, 'webkit': webkit}[sys.argv[1]](Path(sys.argv[2]))
    else:
        main()
