#!/usr/bin/env python3
"""Create the repository-grounded Fedora installation guide."""
from pathlib import Path
from zipfile import ZipFile, ZIP_DEFLATED
from xml.sax.saxutils import escape

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "deliverables" / "focaldesk-fedora-installation-and-configuration-guide.docx"
W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

def para(text="", style=None, bullet=False, code=False, page=False):
    props = []
    if style: props.append(f'<w:pStyle w:val="{style}"/>')
    if page: props.append('<w:pageBreakBefore/>')
    if bullet: text = "• " + text
    rpr = '<w:rPr><w:rFonts w:ascii="Consolas" w:hAnsi="Consolas"/><w:sz w:val="18"/></w:rPr>' if code else ''
    runs = ''.join(f'<w:r>{rpr}<w:t xml:space="preserve">{escape(line)}</w:t></w:r>' + ('' if i == len(text.split("\n"))-1 else '<w:r><w:br/></w:r>') for i, line in enumerate(text.split('\n')))
    return f'<w:p><w:pPr>{"".join(props)}</w:pPr>{runs}</w:p>'

def table(headers, rows, widths):
    borders = ''.join(f'<w:{n} w:val="single" w:sz="4" w:color="D9D9D9"/>' for n in ('top','left','bottom','right','insideH','insideV'))
    grid = ''.join(f'<w:gridCol w:w="{x}"/>' for x in widths)
    def cell(value, width, header=False, shade=False):
        fill = '17365D' if header else ('F3F7FB' if shade else 'FFFFFF')
        rp = '<w:rPr><w:b/><w:color w:val="FFFFFF"/></w:rPr>' if header else ''
        return (f'<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/><w:shd w:fill="{fill}"/>'
                '<w:tcMar><w:top w:w="100" w:type="dxa"/><w:start w:w="120" w:type="dxa"/><w:bottom w:w="100" w:type="dxa"/><w:end w:w="120" w:type="dxa"/></w:tcMar>'
                '<w:vAlign w:val="center"/></w:tcPr><w:p><w:r>' + rp + f'<w:t>{escape(value)}</w:t></w:r></w:p></w:tc>')
    result = f'<w:tbl><w:tblPr><w:tblBorders>{borders}</w:tblBorders></w:tblPr><w:tblGrid>{grid}</w:tblGrid>'
    result += '<w:tr><w:trPr><w:tblHeader/></w:trPr>' + ''.join(cell(x, widths[i], True) for i,x in enumerate(headers)) + '</w:tr>'
    for n, row in enumerate(rows):
        result += '<w:tr>' + ''.join(cell(str(x), widths[i], shade=n % 2 == 1) for i,x in enumerate(row)) + '</w:tr>'
    return result + '</w:tbl>'

def main():
    c = []
    c += [para('FocalDesk Fedora Installation and Configuration Guide', 'Title'),
          para('Repository grounded installation steps configuration files and recovery guidance', 'Subtitle'),
          para('Purpose', 'Heading1'),
          para('This guide identifies the configuration files and installation steps needed to deploy the complete FocalDesk alpha desktop stack on Fedora. The recommended path installs the desktop session, system-wide Fedora user services, AI components, and the credential broker, then uses the existing display manager to select FocalDesk. The native FocalDesk display manager and command automation are deliberately separate opt-in steps because they change login or execution behavior.'),
          para('Scope and safety', 'Heading2'),
          para('FocalDesk is alpha Wayland compositor software. Test its nested backend first, keep a known-working Fedora desktop and recovery TTY available, and do not replace the system display manager until the ordinary FocalDesk Wayland session has been tested. This document describes the repository at the time it was reviewed on 2026 10 24.'),
          para('Installation model', 'Heading1'),
          table(['Layer', 'Recommended Fedora location', 'How it is installed'], [
              ['Desktop compositor and applications', '/usr/local/bin and /usr/libexec', 'just install-desktop'],
              ['Wayland session entry', '/usr/share/wayland-sessions', 'just install-desktop-session'],
              ['Core user services and portal', '/usr/bin and /usr/lib/systemd/user', 'just install-services-fedora'],
              ['AI server console and vector service', '/usr/bin and user-unit path', 'just install-ai-fedora'],
              ['Credential broker PAM and SELinux policy', '/usr/bin /etc and systemd system path', 'just install-secrets-service-fedora'],
              ['Native display manager optional', '/usr/sbin /etc/pam.d and /etc/focaldmd.toml', 'just install-focaldmd-fedora'],
          ], [2100, 2700, 3000]),
          para('Prerequisites', 'Heading1'),
          para('Install Rust through rustup, then install the Fedora development packages below. The project also requires a Wayland host session for nested testing. Vosk voice recognition may require the distribution Vosk package and a separately downloaded model; that model is not part of the repository or installation bundle.'),
          para('sudo dnf install meson ninja-build cmake gcc clang libclang-devel pkgconf-pkg-config libxkbcommon-devel wayland-devel wayland-protocols-devel mesa-libEGL-devel mesa-libGLES-devel libdrm-devel libinput-devel seatd-devel libdisplay-info-devel pipewire-devel alsa-lib-devel polkit-devel pam-devel gtk4-devel gtk4-layer-shell-devel libadwaita-devel poppler-utils xorg-x11-server-Xwayland selinux-policy-devel just', 'Code'),
          para('rustc --version\ncargo --version\njust --version', 'Code'),
          para('Build and prove the safe path', 'Heading1', page=True),
          para('Clone the repository and build the full Rust workspace. Before installing the DRM KMS session, launch FocalDesk nested inside the existing Wayland desktop. A crash then closes only the test window.'),
          para('git clone https://github.com/sjweiler/focaldesk.git\ncd focaldesk\ncargo build --workspace\ncargo run -p focaldesk-desktop --no-default-features --features winit,xwayland', 'Code'),
          para('Optional pre-install checks', 'Heading2'),
          para('Run these before a real session installation when time permits. The hardware paths remain dependent on GPU, driver, display topology, and kernel.'),
          para('cargo fmt --all -- --check\ncargo check --workspace\ncargo clippy --workspace --all-targets\ncargo test --workspace\njust nested-smoke', 'Code'),
          para('Full Fedora installation', 'Heading1'),
          para('Run the following from the repository checkout while logged into the user who will run FocalDesk. The recipe names intentionally use the Fedora variants so binaries and user units are installed system-wide rather than into a per-user development tree.'),
          para('just install-desktop\njust install-desktop-session\njust install-services-fedora\njust install-ai-fedora\njust install-secrets-service-fedora', 'Code'),
          para('What the commands cover', 'Heading2'),
          table(['Command', 'Installs or configures', 'Important behavior'], [
              ['install-desktop', 'Compositor, Settings app, polkit helper, default theme, wallpaper, optional themes', 'Uses /usr/local for desktop apps; builds release artifacts.'],
              ['install-desktop-session', 'FocalDesk Wayland session desktop entry and session target', 'Lets the current display manager present FocalDesk.'],
              ['install-services-fedora', 'Shell, launch, power, notifications, updates, dialogs, controls, settings, portal, remote binary, voice, speech, microphone, connectors, vector, AI server', 'Core units are system-installed but controlled with systemctl --user. Remote desktop remains stopped.'],
              ['install-ai-fedora', 'AI server, dialog service, focal-vector, AI Console, agent skill files', 'Cleans older per-user AI units that could override Fedora units.'],
              ['install-secrets-service-fedora', 'Encrypted credential broker, SELinux policy, runtime directory, system path unit, ACL default', 'Requires selinux-policy-devel. PAM unlock integration is installed only with focaldmd.'],
          ], [1900, 3500, 2400]),
          para('Session activation and verification', 'Heading1'),
          para('Log out of the current desktop, select FocalDesk from the existing display manager’s session chooser, and log in. Do not use sudo with systemctl --user: user services require the logged-in user’s bus and runtime directory.'),
          para('just verify-install system\nsystemctl --user status focaldesk-session.target\nsystemctl --user --failed\ntest -x /usr/local/bin/focaldesk-desktop\ntest -f /usr/share/wayland-sessions/focaldesk.desktop', 'Code'),
          para('The system manifest verifies installed system files and modes. The separate user manifest is for the local development install, so it is not the main verification target for this Fedora system-wide path.'),
          para('Transition from GNOME to FocalDesk', 'Heading1', page=True),
          para('There are two supported transition levels. The lower-risk path leaves Fedora GDM in place and simply selects FocalDesk from GDM’s session chooser. The full replacement path installs focaldmd and changes the system display-manager alias to FocalDesk’s greeter. Neither path requires editing GDM configuration files or Fedora’s shared /etc/pam.d/password-auth and postlogin files.'),
          table(['Change', 'Fedora standard location affected', 'FocalDesk action and reason'], [
              ['Session selection only', 'GDM reads /usr/share/wayland-sessions', 'install-desktop-session writes focaldesk.desktop. GDM can then launch /usr/local/bin/focaldesk-desktop without replacing GDM.'],
              ['Display manager replacement', 'systemd display-manager.service alias', 'install-focaldmd-fedora installs focaldmd.service with Alias=display-manager.service. Enabling it takes the active display-manager role from GDM.'],
              ['Human login PAM entry', '/etc/pam.d/focaldmd', 'The recipe installs a FocalDesk-specific PAM service. It reuses Fedora password-auth and postlogin stacks and adds pam_focald_secrets.so; it does not modify those shared stacks.'],
              ['Greeter PAM entry', '/etc/pam.d/focaldmd-greeter', 'A non-interactive greeter policy establishes the greeter logind session with pam_systemd.'],
              ['Credential provider activation', 'D-Bus Secret Service activation metadata', 'The secrets recipe removes stale org.freedesktop.secrets activation files so a keyless broker or old provider cannot activate incorrectly.'],
          ], [1850, 2500, 4000]),
          para('Recommended GNOME migration sequence', 'Heading2'),
          para('First validate FocalDesk from GDM. Only after that succeeds, install the native display manager and credential PAM integration. The recipe creates the focaldm greeter user, installs /etc/focaldmd.toml, /etc/pam.d/focaldmd, /etc/pam.d/focaldmd-greeter, /usr/lib/systemd/system/focaldmd.service, and the PAM module. It does not enable focaldmd automatically.'),
          para('just install-focaldmd-fedora\n# Confirm the new unit is installed before changing the active display manager:\nsystemctl cat focaldmd.service\nsystemctl status gdm.service\n# Switch only after a tested FocalDesk login and with a recovery TTY available:\nsudo systemctl disable --now gdm.service\nsudo systemctl enable --now focaldmd.service', 'Code'),
          para('The last two commands are the material Fedora configuration change: focaldmd becomes display-manager.service. They stop GDM, so run them from a TTY or a prepared recovery session rather than relying on the graphical desktop that is being replaced. Do not delete GDM or alter its packaged files; keeping it installed makes rollback straightforward.'),
          para('Credential manager migration from GNOME Keyring', 'Heading1'),
          para('GNOME Keyring and focald-secrets cannot simultaneously own org.freedesktop.secrets in the same user session. Before switching display managers, review and optionally import existing GNOME Keyring entries while GNOME Keyring is still active. The import copies readable items without deleting or modifying the originals and is safe to rerun.'),
          para('focald-secrets-import-gnome-keyring --dry-run\nfocald-secrets-import-gnome-keyring --import', 'Code'),
          para('With focaldmd, the FocalDesk-specific PAM policy invokes pam_focald_secrets.so during authentication, password handling, and session setup. The root-only credential staging path triggers focald-secrets@UID.path; PID 1 passes the unlocked key into the broker’s private credential store. This avoids a second keyring prompt and lets focald-secrets own the standard Secret Service API. The FocalDesk PAM policy deliberately does not launch pam_gnome_keyring.so.'),
          para('After the first focaldmd login, verify the active service and Secret Service owner. These checks do not reveal credential values.'),
          para('uid="$(id -u)"\nsystemctl is-active "focald-secrets@${uid}.path"\nsystemctl is-active "focald-secrets@${uid}.service"\ntest -S "/run/user/${uid}/focaldesk/secrets.sock"\nbusctl --user status org.freedesktop.secrets\nbash scripts/check-secrets-health.sh', 'Code'),
          para('If you keep GDM, it remains responsible for the login PAM path and does not automatically activate the focaldmd-specific unlock flow. The system credential broker can still be installed, but the seamless password-unlock behavior documented here requires focaldmd’s PAM policy. Use the documented explicit development or recovery key flow only for development sessions; do not weaken the production broker to a basic Chrome password store.'),
          para('Rollback to GNOME', 'Heading2'),
          para('From a TTY, reverse the display-manager switch. GDM’s package and configuration remain intact because the FocalDesk installation uses a separate service and PAM entries.'),
          para('sudo systemctl disable --now focaldmd.service\nsudo systemctl enable --now gdm.service\nsudo systemctl status display-manager.service', 'Code'),
          para('Components intentionally not enabled by the main bundle', 'Heading1', page=True),
          table(['Component', 'Status after full base install', 'Enable only when ready'], [
              ['Remote desktop', 'Binary and unit installed but disabled and stopped', 'Read docs/remote-desktop.md, then start the user unit explicitly. It is experimental and view-only.'],
              ['Command automation', 'Not installed by install-services-fedora', 'Review automation scripts and unit, then run just install-automation-service-fedora.'],
              ['Native focaldmd display manager', 'Not installed or enabled', 'Test the ordinary session first, then run just install-focaldmd-fedora.'],
              ['Focaldmd activation', 'Installed recipe does not enable or start it', 'Only after tested: sudo systemctl enable --now focaldmd.service. This changes display-manager.service.'],
          ], [1900, 2350, 3550]),
          para('Native display manager configuration', 'Heading2'),
          para('The optional focaldmd recipe installs /etc/focaldmd.toml, PAM policies, the greeter, a system unit, and the focaldm system user. The initial configuration selects renderer = "gles", the documented recovery default. Keep it while validating a system; set renderer = "vulkan" only after the Vulkan DRM path has been installed and tested. Preserve unknown settings and create a recovery copy before manual edits.'),
          para('sudo cp -a /etc/focaldmd.toml /etc/focaldmd.toml.backup\nsudoedit /etc/focaldmd.toml\n# keep or restore: renderer = "gles"\nsudo systemctl restart focaldmd.service', 'Code'),
          para('Configuration inventory', 'Heading1'),
          para('FocalDesk follows the XDG base directory specification. When XDG_CONFIG_HOME, XDG_DATA_HOME, or XDG_STATE_HOME are unset, use ~/.config, ~/.local/share, and ~/.local/state respectively.'),
          table(['Path', 'Owner and purpose', 'Change guidance'], [
              ['$XDG_CONFIG_HOME/focaldesk/settings.json', 'Canonical desktop, shell, app, input, workspace, privacy, power, debug, and chrome settings', 'Use the Settings app. For a manual edit, stop the owning component, make a backup, validate JSON, then reload/restart it.'],
              ['$XDG_CONFIG_HOME/focaldesk/config.toml', 'Legacy typed shell configuration', 'Migrated once into settings.json and retained as recovery material; do not treat as canonical.'],
              ['$XDG_CONFIG_HOME/focaldesk/displays.json', 'Detected output topology and runtime display choices', 'Generated. Never edit while FocalDesk is running.'],
              ['$XDG_DATA_HOME/focaldesk/themes/', 'Imported theme packages', 'Use Theme Editor import and uninstall rather than filesystem edits.'],
              ['$XDG_CONFIG_HOME/focaldesk/ai_permissions.toml and ai_console.json', 'AI decisions and console data, mode 0600', 'Manage through the permission UI and AI Console where possible.'],
              ['$XDG_CONFIG_HOME/focaldesk/automation/', 'Automation definitions and Lua scripts', 'Security-sensitive experimental opt-in; review before enabling automation.'],
              ['$XDG_CONFIG_HOME/focaldesk/secrets-acl.toml or /etc/focaldesk/secrets-acl.toml', 'Credential-broker service access policy', 'Security-sensitive default-deny policy; do not broadly relax it.'],
              ['$XDG_STATE_HOME/focaldesk/session.json', 'Restore-session snapshot', 'Generated atomically; disabling Restore session removes it.'],
              ['/etc/focaldmd.toml', 'Optional native display manager and renderer choice', 'Edit only after focaldmd is installed; keep a working alternate login.'],
          ], [2400, 2700, 2700]),
          para('Safe settings change procedure', 'Heading1'),
          para('Prefer the Settings application. If a manual change is necessary, first create a user-owned recovery copy and check JSON syntax. Do not overwrite the whole document simply to change one setting; retain unknown keys. The FocalDesk CLI can reload settings only after a valid persistent change.'),
          para('config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/focaldesk"\ncp -a "$config_dir/settings.json" "$config_dir/settings.json.backup"\npython3 -m json.tool "$config_dir/settings.json" >/dev/null\nfocaldesk-cli reload-settings', 'Code'),
          para('The shell panel and dock layout is stored inside desktop_config in settings.json. Restart the shell clients after geometry changes. Display topology settings are persisted separately in displays.json and should be changed through Settings while the compositor is managing outputs.'),
          para('Operational notes', 'Heading1'),
          table(['Area', 'Guidance'], [
              ['Renderer', 'GLES is the recovery and compatibility default. Raw Ash Vulkan is a direct DRM KMS renderer and is hardware-sensitive; retain GLES as a known-good fallback.'],
              ['Secrets', 'Do not put API keys in settings, screenshots, logs, or the repository. Cloud provider keys are managed by focald-secrets; environment variables are development or upgrade fallbacks.'],
              ['Logs', 'Compositor logging resolves through FOCALDESK_LOG_FILE, XDG state, XDG cache, ~/.cache, then /tmp. User-service logs are available through journalctl --user. Review diagnostics before sharing.'],
              ['Updates', 'Do not mix Fedora system-installed user units with older ~/.config/systemd/user development units. The Fedora AI recipe handles this migration for AI components.'],
          ], [1750, 6250]),
          para('Troubleshooting and recovery', 'Heading1'),
          para('If the FocalDesk session produces a blank display, switch to another virtual terminal using the distribution’s Ctrl Alt F key sequence, log in, and inspect the current boot journal. Start with one SDR display at 60 Hz and reintroduce complexity one change at a time. Do not use undocumented HDR or atomic-commit overrides as a general fix.'),
          para('journalctl -b -t focaldesk\njournalctl --user -u focaldesk-server.service -b\nsystemctl --user --failed\nfocaldesk-cli diagnostics --no-logs', 'Code'),
          para('For a broken manual settings change, restore the recovery copy from a working session, validate it, and restart only the affected user service. For display-manager failure, select the previous display manager or use a TTY; do not enable focaldmd until its ordinary FocalDesk session has been proven stable.'),
          para('Removal', 'Heading2'),
          para('The repository uninstaller removes only paths recorded in the reviewed install manifests and preserves user settings, secrets, themes, logs, and state by default. Purging user state is irreversible.'),
          para('just uninstall system\n# Destructive user-state removal only after an explicit backup decision:\njust uninstall-purge', 'Code'),
          para('Repository sources', 'Heading1'),
          para('This guide is derived from the primary project materials: README.md; docs/building.md; docs/configuration.md; docs/troubleshooting.md; docs/secrets.md; docs/remote-desktop.md; justfile; packaging/install-manifest-system.txt; packaging/focaldesk/focaldmd.toml; and the systemd, PAM, portal, SELinux, and Wayland-session packaging files.')]

    body = ''.join(c) + '<w:sectPr><w:footerReference w:type="default" r:id="rId1"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1050" w:right="1080" w:bottom="1050" w:left="1080" w:header="720" w:footer="720"/></w:sectPr>'
    styles = f'''<w:styles xmlns:w="{W}"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Aptos" w:hAnsi="Aptos"/><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:pPr><w:spacing w:after="125" w:line="276" w:lineRule="auto"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:pPr><w:spacing w:after="180"/></w:pPr><w:rPr><w:b/><w:sz w:val="38"/><w:color w:val="000000"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Subtitle"><w:name w:val="Subtitle"/><w:pPr><w:spacing w:after="340"/></w:pPr><w:rPr><w:sz w:val="22"/><w:color w:val="404040"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:pPr><w:keepNext/><w:spacing w:before="280" w:after="130"/></w:pPr><w:rPr><w:b/><w:sz w:val="29"/><w:color w:val="000000"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:pPr><w:keepNext/><w:spacing w:before="200" w:after="95"/></w:pPr><w:rPr><w:b/><w:sz w:val="24"/><w:color w:val="000000"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Code"><w:name w:val="Code"/><w:basedOn w:val="Normal"/><w:pPr><w:spacing w:before="65" w:after="160"/><w:shd w:fill="F3F7FB"/><w:ind w:left="180" w:right="180"/></w:pPr><w:rPr><w:rFonts w:ascii="Consolas" w:hAnsi="Consolas"/><w:sz w:val="18"/><w:color w:val="202020"/></w:rPr></w:style></w:styles>'''
    doc = f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}" xmlns:r="{R}"><w:body>{body}</w:body></w:document>'
    types = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/></Types>'''
    rootrels = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>'''
    rels = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>'''
    footer = f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:ftr xmlns:w="{W}"><w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:sz w:val="18"/><w:color w:val="666666"/></w:rPr><w:t>FocalDesk Fedora Installation and Configuration Guide</w:t></w:r></w:p></w:ftr>'
    OUT.parent.mkdir(exist_ok=True)
    with ZipFile(OUT, 'w', ZIP_DEFLATED) as z:
        z.writestr('[Content_Types].xml', types); z.writestr('_rels/.rels', rootrels)
        z.writestr('word/document.xml', doc); z.writestr('word/styles.xml', styles)
        z.writestr('word/footer1.xml', footer); z.writestr('word/_rels/document.xml.rels', rels)
    print(OUT)

if __name__ == '__main__':
    main()
