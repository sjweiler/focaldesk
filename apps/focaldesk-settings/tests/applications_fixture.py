"""Private XDG desktop registry for real GIO association tests."""
import os

EDITOR = 'org.focaldesk.fixture-editor.desktop'
OTHER = 'org.focaldesk.fixture-other.desktop'


def install(root, env):
    applications = root / 'data/applications'
    applications.mkdir(parents=True)
    canary = root / 'application-canary'
    canary.write_text('#!/bin/sh\ntouch "' + str(root / 'application-launched') + '"\n')
    canary.chmod(0o700)
    for identifier, name in [(EDITOR, 'Fixture Editor'), (OTHER, 'Fixture Other')]:
        (applications / identifier).write_text(
            '[Desktop Entry]\nType=Application\nName=' + name + '\nExec=' + str(canary)
            + ' %U\nMimeType=text/plain;application/pdf;x-scheme-handler/https;\n')
    (applications / 'mimeinfo.cache').write_text(
        '[MIME Cache]\ntext/plain=' + EDITOR + ';' + OTHER + ';\n')
    (root / 'config/mimeapps.list').write_text(
        '[Default Applications]\ntext/plain=' + EDITOR + ';\napplication/pdf=' + EDITOR
        + ';\n\n[Fixture Unrelated]\nKeep=yes\n')
    # Keep system GTK schemas and MIME descriptions, while every write is private.
    env['XDG_CURRENT_DESKTOP'] = 'FocalDesk'
    env['XDG_CONFIG_DIRS'] = str(root / 'system-config')
    env['XDG_DATA_DIRS'] = env.get('XDG_DATA_DIRS', '/usr/local/share:/usr/share')
    return canary
