#!/usr/bin/env python3
"""Build a concise, source-grounded engineering guide for FocalDesk."""
from pathlib import Path
from zipfile import ZipFile, ZIP_DEFLATED
from xml.sax.saxutils import escape
import shutil

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "deliverables" / "focaldesk-engineering-guide.docx"

NS = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

def p(text="", style=None, bold_prefix=None, bullet=False, page_break=False):
    props = ""
    if style: props += f'<w:pStyle w:val="{style}"/>'
    # Use a literal bullet so this small standalone OOXML package needs no numbering part.
    if bullet: text = "• " + text
    if page_break: props += '<w:pageBreakBefore/>'
    runs = ""
    if bold_prefix and text.startswith(bold_prefix):
        runs += f'<w:r><w:rPr><w:b/></w:rPr><w:t>{escape(bold_prefix)}</w:t></w:r>'
        text = text[len(bold_prefix):]
    for i, line in enumerate(text.split("\n")):
        if i: runs += '<w:r><w:br/></w:r>'
        runs += f'<w:r><w:t xml:space="preserve">{escape(line)}</w:t></w:r>'
    return f'<w:p><w:pPr>{props}</w:pPr>{runs}</w:p>'

def table(headers, rows, widths):
    grid = ''.join(f'<w:gridCol w:w="{w}"/>' for w in widths)
    def cell(text, width, header=False):
        shade = '<w:shd w:fill="17365D"/>' if header else ''
        rpr = '<w:b/><w:color w:val="FFFFFF"/>' if header else ''
        return (f'<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/>{shade}'
                '<w:tcMar><w:top w:w="90" w:type="dxa"/><w:start w:w="110" w:type="dxa"/>'
                '<w:bottom w:w="90" w:type="dxa"/><w:end w:w="110" w:type="dxa"/></w:tcMar></w:tcPr>'
                f'<w:p><w:r><w:rPr>{rpr}</w:rPr><w:t>{escape(text)}</w:t></w:r></w:p></w:tc>')
    out = '<w:tbl><w:tblPr><w:tblBorders>' + ''.join(
        f'<w:{x} w:val="single" w:sz="4" w:color="D9D9D9"/>' for x in ['top','left','bottom','right','insideH','insideV']
    ) + '</w:tblBorders></w:tblPr><w:tblGrid>' + grid + '</w:tblGrid>'
    out += '<w:tr>' + ''.join(cell(x, widths[i], True) for i,x in enumerate(headers)) + '</w:tr>'
    for row in rows:
        out += '<w:tr>' + ''.join(cell(str(x), widths[i]) for i,x in enumerate(row)) + '</w:tr>'
    return out + '</w:tbl>'

def image(rel_id, name, docpr_id, width_emu, height_emu):
    return f'''<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:drawing>
<wp:inline xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" distT="0" distB="0" distL="0" distR="0">
<wp:extent cx="{width_emu}" cy="{height_emu}"/><wp:docPr id="{docpr_id}" name="{name}"/>
<a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="0" name="{name}"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="{rel_id}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{width_emu}" cy="{height_emu}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic>
</wp:inline></w:drawing></w:r></w:p>'''

def main():
    content = []
    content.append(p("FocalDesk Engineering Guide", "Title"))
    content.append(p("Codebase architecture crates rendering and engineering questions", "Subtitle"))
    content.append(p("Purpose", "Heading1"))
    content.append(p("This guide explains how the FocalDesk Wayland desktop environment is organized, how a frame reaches the display, and where an engineer should start when changing behavior. The principal conclusion is that FocalDesk is deliberately split: the compositor owns time-sensitive display and input work, while desktop capabilities live in separate typed services. The rendering stack has three distinct implementations and they should not be treated as interchangeable."))
    content.append(p("Scope and status", "Heading2"))
    content.append(p("The repository describes alpha software. The GLES DRM path remains the compatibility and recovery default; raw Ash Vulkan is the production-oriented DRM backend; wgpu Vulkan is a nested development backend. HDR, color management, PipeWire capture, remote desktop, and AI automation contain experimental or hardware-dependent areas."))
    content.append(p("Architecture at a glance", "Heading1"))
    content.append(image("rId2", "Architecture overview", 1, 5486400, 3086100))
    content.append(p("Applications submit Wayland or XWayland surfaces. The compositor owns surface lifecycle, workspace/window state, focus, input routing, damage, output coordination, and presentation. Shell applications and long-running system capabilities are separate processes communicating through typed IPC, preventing slow or failed service work from blocking the frame loop."))
    content.append(p("Runtime responsibilities", "Heading2"))
    content.append(table(["Layer", "Owns", "Does not own"], [
        ["Compositor engine", "Wayland protocol, scene state, input, layout, rendering, output/KMS coordination", "Long-running launch, updates, notification persistence, policy UI"],
        ["Desktop services", "Launch, power, notifications, updates, dialogs, controls, AI/automation workflows", "Direct compositor frame-loop authority"],
        ["Client applications", "Surface buffers and protocol commits", "Window placement or scanout ownership"],
    ], [1700, 3900, 1900]))
    content.append(p("Important code boundaries", "Heading2"))
    for x in [
        "focaldesk-types contains shared identifiers and backend identity.",
        "focaldesk-input normalizes events, physical/logical geometry, transforms, pointer scaling, and optional Smithay translation.",
        "focaldesk-shortcuts parses XKB key symbols and turns normalized combinations into backend-neutral desktop actions. It intentionally has no Smithay or focaldesk-types dependency.",
        "focaldesk-engine performs focus, grabs, hit testing, Wayland delivery, lock-screen routing, action execution, and backend orchestration.",
        "focaldesk-ipc defines private typed request, response, and event boundaries used by services and shell clients.",
    ]: content.append(p(x, bullet=True))
    content.append(p("Crates and dependency roles", "Heading1", page_break=True))
    content.append(p("The workspace is a Rust Cargo workspace. Crates are intentionally separated by domain so dependencies do not force compositor internals into every application. The table names the high-value crates; apps and daemons consume these libraries rather than duplicating core logic."))
    content.append(table(["Crate family", "Representative crates", "Responsibility and key dependencies"], [
        ["Core model and input", "focaldesk-types, focaldesk-input, focaldesk-shortcuts", "Shared types; normalized input and transforms; XKB key parsing and binding resolution. Smithay is optional at the input translation edge."],
        ["Compositor and rendering", "focaldesk-engine, focaldesk-render, focaldesk-cursor, focaldesk-resources", "Protocol and state orchestration; renderer-neutral primitives; cursor and asset/font/SVG loading. Smithay, Ash, wgpu, Naga, font and raster libraries."],
        ["System contracts", "focaldesk-ipc, focaldesk-config, focaldesk-settings-core, focaldesk-logging", "Typed local IPC, persisted configuration, settings model, structured tracing/Journald integration."],
        ["Desktop services", "focal-launchd, focaldesk-powerd, focaldesk-notificationsd, focaldesk-dialogd, focaldesk-controlsd", "Recoverable user services for actions outside the frame loop; each owns its respective state and policy boundary."],
        ["Experience and clients", "focaldesk-ui, focaldesk-gtk, focaldesk-shell-client, focaldesk-shell-gtk", "Native UI/state plus GTK shell clients and optional VUI presentation paths."],
        ["Security and automation", "focaldesk-permissions, focaldesk-secrets-client, focald-secrets, focaldesk-ai, focaldesk-automation", "Capability/policy data, encrypted secret brokerage, permission-aware AI and Lua automation workflows."],
        ["Optional capabilities", "focaldesk-remote-protocol, focaldesk-remoted, focaldesk-voice, focaldesk-network", "View-only remote desktop protocol, speech recognition, network state/control."],
    ], [1850, 2350, 3300]))
    content.append(p("External dependencies worth knowing", "Heading2"))
    for x in [
        "Smithay provides Rust Wayland-compositor building blocks: protocol handling, desktop abstractions, input/backend integration, GLES rendering integration, DRM/KMS ownership, and XWayland support.",
        "Ash exposes Vulkan directly. It is used for the DRM Vulkan path where FocalDesk needs exact control over external memory, queue ownership, synchronization, scanout formats, and atomic KMS handoff.",
        "wgpu is limited to the nested Vulkan development renderer; it is not the production DRM Vulkan compositor path.",
        "GBM, DRM/KMS, libinput, libseat, xkbcommon, PipeWire, and Wayland protocols are system-facing dependencies; their availability and driver behavior influence real-hardware support.",
        "serde, tokio, tracing, zbus, rusqlite, cryptographic crates, GTK4/libadwaita, and text/SVG/image libraries support services and applications rather than the scanout hot path.",
    ]: content.append(p(x, bullet=True))
    content.append(p("Rendering and presentation", "Heading1", page_break=True))
    content.append(image("rId3", "Rendering pipeline", 2, 5486400, 3086100))
    content.append(p("Frame lifecycle", "Heading2"))
    for x in [
        "A client commits a Wayland or XWayland surface. The compositor updates surface state and propagates buffer damage through transforms, scaling, viewport state, subsurfaces, and placement.",
        "The engine constructs output-local scene order: wallpaper and shell base, client windows/popups/layer-shell content, shell overlays, and cursor. The cursor stays foremost.",
        "Damage is compacted per output. Retained targets and bounded per-buffer history let an acquired scanout buffer repaint only regions it missed; uncertain history, resize, rearrangement, or unsupported commits safely fall back to a larger or full repaint.",
        "The selected renderer composes into its target, performs color conversion/output encoding, synchronizes GPU work, and hands a completed buffer plus fences to DRM/KMS or the nested host window.",
        "KMS presents at vblank. Frame callbacks and release signaling let clients reuse buffers without compositor-thread GPU-idle waits.",
    ]: content.append(p(x, bullet=True))
    content.append(p("Three renderer paths", "Heading2"))
    content.append(table(["Path", "Where it runs", "Engineering meaning"], [
        ["Smithay GLES", "Native DRM/KMS and nested compatibility path", "Established compatibility/recovery renderer; has broad integration and retains HDR and zero-copy DMA-BUF capture capabilities."],
        ["Raw Ash Vulkan", "Native DRM/KMS", "Production-oriented Vulkan path. Smithay remains DRM/KMS owner; GBM buffers are Vulkan color attachments; Vulkan exports sync files to atomic KMS. No Vulkan WSI swapchain, EGL bridge, or GLES copy."],
        ["wgpu Vulkan", "Nested winit development backend", "Validates Vulkan scene construction, texture/DMA-BUF import, transforms, input, and retained damage handling without risking the active KMS session. Not the production DRM renderer."],
    ], [1500, 1850, 4150]))
    content.append(p("Color capture and HDR", "Heading2"))
    content.append(p("The compositor retains a linear-light FP16 scene where available. Client buffers are decoded according to their declared transfer/gamut state, then wallpaper, shell, and effects are composed. SDR output uses an output transform and optional 33 cubed ICC LUT. HDR10 output prefers 10-bit GBM scanout, maps the scene to BT.2020, encodes ST 2084 PQ, and stages static HDR metadata through atomic KMS. If a 10-bit buffer or connector signaling cannot be established, the output remains SDR rather than presenting un-signaled PQ pixels."))
    content.append(p("Portal capture deliberately branches before monitor-specific encoding. Because the capture transport lacks a reliable color-description channel, the current public contract is SDR sRGB/Rec.709. Wide-gamut/HDR pixels are converted and tone-mapped to that contract, avoiding an untagged PQ or Display P3 buffer that a consumer would misinterpret."))
    content.append(p("Standard engineering questions", "Heading1"))
    qas = [
        ("Why Rust and Smithay?", "Rust provides memory-safety and concurrency guarantees in a component that touches untrusted buffers, devices, IPC, and asynchronous services. Smithay supplies compositor building blocks without imposing a full desktop product or a C integration boundary."),
        ("Why retain GLES after adding Vulkan?", "GLES is the recovery and compatibility baseline. Vulkan adds control over external memory, synchronization, FP16/HDR composition, and KMS handoff, but it does not make HDR inherently possible. Keeping both paths is intentional operational risk management."),
        ("How does FocalDesk avoid a monolithic compositor?", "The compositor is kept focused on surfaces, input, rendering, and session behavior. Launching, power, notifications, updates, dialogs, controls, AI, and automation live behind typed IPC in separate services, so their failures and latency need not stall frames."),
        ("What protects local services?", "Private sockets use Linux peer credentials. Sensitive endpoints add deny-by-default caller policy based on peer executable and systemd cgroup unit. This limits unrelated applications in the same session, while not claiming that a user's writable binaries are a complete security boundary."),
        ("How is performance protected?", "Damage tracking, retained targets, cached shell/font/wallpaper assets, per-output state, GPU fence-based progress, and conservative fallbacks limit unnecessary work. The project exposes timing and damage debug controls for measurement rather than assuming improvements."),
        ("What happens when rendering or display state fails?", "The architecture favors bounded recovery: GPU fence stalls trigger in-process Vulkan device/scanout recreation; missing vblank rebuilds KMS scanout; HDR-property loss is retried then bounded by output rebuild. Uncertain damage causes safe larger repaint rather than visual corruption."),
        ("What should a new engineer run first?", "Build the workspace, then use the established nested winit path rather than replacing the active desktop. Use nested-wgpu only when working on the experimental Vulkan backend. Check crate boundaries with just architecture-check and use targeted smoke/regression scripts before testing DRM hardware."),
        ("What are the current engineering risks?", "Alpha API/configuration stability, multi-GPU/driver variance, hardware-dependent HDR/capture behavior, incomplete YUV and disjoint DMA-BUF support, and renderer feature-parity gaps. Treat production claims as hardware- and path-specific."),
    ]
    for q,a in qas:
        content.append(p(q, "Heading2")); content.append(p(a))
    content.append(p("Code navigation and change guide", "Heading1"))
    content.append(table(["If you need to change", "Start here", "Check next"], [
        ["Keyboard/pointer semantics", "crates/focaldesk-input and focaldesk-shortcuts", "Engine routing and focus/grab behavior; preserve one-time modifier conversion."],
        ["Wayland window/workspace behavior", "crates/focaldesk-engine core and backend modules", "Damage, placement, XWayland/layer-shell cases, and nested smoke tests."],
        ["GPU renderer behavior", "crates/focaldesk-render plus engine drm_vulkan or wgpu_nested", "Synchronization, buffer lifetime, transforms, output recovery, and exact target backend."],
        ["Display color/HDR/capture", "docs/rendering.md and docs/hdr.md, then engine renderer code", "Output signaling, SDR fallback, monitor ICC data, and capture contract."],
        ["Desktop service behavior", "services and their focaldesk-ipc message types", "Caller authorization, timeouts/retries, and compositor non-blocking behavior."],
    ], [1900, 2700, 2900]))
    content.append(p("Validation checklist", "Heading2"))
    for x in [
        "Run formatting, focused unit tests, and the workspace checks relevant to the changed crate.",
        "Run just architecture-check after changing crate dependencies or input/shortcut boundaries.",
        "Use nested smoke tests for normal compositor behavior. Use nested-wgpu smoke only for the experimental wgpu Vulkan backend.",
        "For DRM/HDR/KMS changes, preserve a recovery session and run the repository's hardware regression scripts on representative hardware.",
        "For IPC or permission changes, test denied callers as well as authorized flows; correctness includes refusal behavior.",
    ]: content.append(p(x, bullet=True))
    # An explicit run-level break is honored consistently by Writer and Word.
    content.append('<w:p><w:r><w:br w:type="page"/></w:r></w:p>')
    content.append(p("Sources in this repository", "Heading1"))
    content.append(p("The following primary repository materials underpin this guide. Repository state reviewed on 2026 10 24."))
    content.append(table(["Source", "Use in this guide"], [
        ["README.md", "Product scope, alpha status, feature and compatibility framing."],
        ["Cargo.toml and crate manifests", "Workspace membership and direct dependency roles."],
        ["docs/architecture.md", "System boundaries, input/shortcut ownership, and architecture diagram."],
        ["docs/rendering.md", "Renderer paths, buffer lifecycle, damage, color, HDR, KMS, and capture behavior."],
        ["docs/engineering.md", "Rationale for Rust, Smithay, GLES, Vulkan, and session lifecycle choices."],
        ["docs/ipc.md", "Service boundaries, message flow, and caller authorization approach."],
        ["docs/building.md and scripts/check-crate-boundaries.py", "Developer entry points, backend caveats, and enforced dependency boundaries."],
    ], [2450, 5050]))

    body = ''.join(content) + '''<w:sectPr><w:footerReference w:type="default" r:id="rId1"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1080" w:right="1080" w:bottom="1080" w:left="1080" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr>'''
    styles = '''<w:styles xmlns:w="%s"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Aptos" w:hAnsi="Aptos"/><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:pPr><w:spacing w:after="130" w:line="276" w:lineRule="auto"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:pPr><w:spacing w:after="160"/></w:pPr><w:rPr><w:b/><w:sz w:val="40"/><w:color w:val="000000"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Subtitle"><w:name w:val="Subtitle"/><w:pPr><w:spacing w:after="360"/></w:pPr><w:rPr><w:sz w:val="22"/><w:color w:val="404040"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:pPr><w:keepNext/><w:spacing w:before="300" w:after="150"/></w:pPr><w:rPr><w:b/><w:sz w:val="30"/><w:color w:val="000000"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:pPr><w:keepNext/><w:spacing w:before="200" w:after="100"/></w:pPr><w:rPr><w:b/><w:sz w:val="24"/><w:color w:val="000000"/></w:rPr></w:style></w:styles>''' % NS
    document = f'''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{NS}" xmlns:r="{R}" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" mc:Ignorable="w14 wp14"><w:body>{body}</w:body></w:document>'''
    rels = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/architecture-overview.png"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/rendering-pipeline.png"/></Relationships>'''
    content_types = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/></Types>'''
    rootrels = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>'''
    footer = f'''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:ftr xmlns:w="{NS}"><w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:color w:val="666666"/><w:sz w:val="18"/></w:rPr><w:t>FocalDesk Engineering Guide</w:t></w:r></w:p></w:ftr>'''
    OUT.parent.mkdir(exist_ok=True)
    with ZipFile(OUT, 'w', ZIP_DEFLATED) as z:
        z.writestr('[Content_Types].xml', content_types); z.writestr('_rels/.rels', rootrels)
        z.writestr('word/document.xml', document); z.writestr('word/styles.xml', styles); z.writestr('word/footer1.xml', footer); z.writestr('word/_rels/document.xml.rels', rels)
        z.write(ROOT/'docs/diagrams/architecture-overview.png', 'word/media/architecture-overview.png')
        z.write(ROOT/'docs/diagrams/rendering-pipeline.png', 'word/media/rendering-pipeline.png')
    print(OUT)

if __name__ == '__main__': main()
