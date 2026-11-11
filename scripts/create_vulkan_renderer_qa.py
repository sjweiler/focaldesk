#!/usr/bin/env python3
"""Create a source-grounded DOCX Q&A for FocalDesk's Vulkan renderer."""
from pathlib import Path
from zipfile import ZipFile, ZIP_DEFLATED
from xml.sax.saxutils import escape

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "deliverables" / "focaldesk-vulkan-renderer-engineering-qa.docx"
NS = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

def para(text="", style=None, bold_prefix=None, page_break=False):
    props = '<w:keepLines/>'
    if style:
        props += f'<w:pStyle w:val="{style}"/>'
    if page_break:
        props += '<w:pageBreakBefore/>'
    runs = ""
    if bold_prefix and text.startswith(bold_prefix):
        runs += f'<w:r><w:rPr><w:b/></w:rPr><w:t>{escape(bold_prefix)}</w:t></w:r>'
        text = text[len(bold_prefix):]
    for i, line in enumerate(text.split("\n")):
        if i:
            runs += '<w:r><w:br/></w:r>'
        runs += f'<w:r><w:t xml:space="preserve">{escape(line)}</w:t></w:r>'
    return f'<w:p><w:pPr>{props}</w:pPr>{runs}</w:p>'

def table(headers, rows):
    widths = [1850, 5550]
    grid = ''.join(f'<w:gridCol w:w="{w}"/>' for w in widths)
    def cell(text, width, header=False):
        shade = '<w:shd w:fill="17365D"/>' if header else ''
        rpr = '<w:b/><w:color w:val="FFFFFF"/>' if header else ''
        return (f'<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/>{shade}'
                '<w:tcMar><w:top w:w="90" w:type="dxa"/><w:start w:w="110" w:type="dxa"/>'
                '<w:bottom w:w="90" w:type="dxa"/><w:end w:w="110" w:type="dxa"/></w:tcMar>'
                '</w:tcPr><w:p><w:r><w:rPr>' + rpr + f'</w:rPr><w:t>{escape(text)}</w:t></w:r></w:p></w:tc>')
    borders = ''.join(f'<w:{side} w:val="single" w:sz="4" w:color="D9D9D9"/>'
                      for side in ('top', 'left', 'bottom', 'right', 'insideH', 'insideV'))
    body = '<w:tbl><w:tblPr><w:tblBorders>' + borders + '</w:tblBorders></w:tblPr><w:tblGrid>' + grid + '</w:tblGrid>'
    body += '<w:tr>' + ''.join(cell(x, widths[i], True) for i, x in enumerate(headers)) + '</w:tr>'
    for row in rows:
        body += '<w:tr>' + ''.join(cell(x, widths[i]) for i, x in enumerate(row)) + '</w:tr>'
    return body + '</w:tbl>'

def main():
    content = []
    content.append(para('FocalDesk Vulkan Renderer Engineering Questions and Answers', 'Title'))
    content.append(para('Source grounded review of the production raw Ash DRM renderer', 'Subtitle'))
    content.append(para('Purpose and conclusion', 'Heading1'))
    content.append(para('This document answers standard engineering questions about the FocalDesk Vulkan renderer by reading the repository implementation and its renderer design notes. The key conclusion is that the production Vulkan path is a raw Ash compositor that renders into compositor owned GBM DMA BUF targets and hands them to KMS. It deliberately does not use Vulkan WSI or a Vulkan swapchain. The wgpu Vulkan renderer is a separate nested development backend.'))
    content.append(para('Scope', 'Heading2'))
    content.append(para('The answers focus on crates/focaldesk-render/src/ash_drm.rs and the rendering contract in docs/rendering.md. Code behavior is described as implemented or documented in this checkout, not as a claim of hardware validation on every driver.'))
    content.append(para('Renderer at a glance', 'Heading1'))
    content.append(table(['Area', 'Implemented design'], [
        ('Presentation ownership', 'Smithay and DRM KMS own scanout. Ash imports GBM DMA BUF targets, renders to them, releases ownership to the foreign queue family, and exports a sync file for atomic KMS.'),
        ('Primary render targets', 'A retained FP16 R16G16B16A16 SFLOAT scene is composed first. A second output pass applies the output transform and LUT into the imported scanout target.'),
        ('Buffer inputs', 'Client DMA BUFs can be imported as external Vulkan images with DRM modifiers. SHM and unsupported imports use staging uploads when pixel data is available.'),
        ('Synchronization', 'DMA BUF writer fences become temporary Vulkan wait semaphores. Submission signals an exportable sync file semaphore for KMS; completion fences retain resources until the GPU is done.'),
        ('Operational posture', 'GLES remains the compatibility and recovery renderer. Raw Vulkan has bounded recovery for GPU or presentation stalls; wedged-device teardown can be skipped.'),
    ]))
    content.append(para('Architecture and device questions', 'Heading1', page_break=True))
    qas = [
        ('What is the production Vulkan renderer and where is it implemented?', 'The production DRM renderer is AshDrmRenderer in crates/focaldesk-render/src/ash_drm.rs. It is compiled behind the ash-drm feature. The module header says raw Vulkan renders compositor owned DRM GBM scanout buffers. The renderer boundary exposes compositor concepts such as TextureQuad and DrmRenderTarget rather than exposing raw wgpu types.'),
        ('Does it use a Vulkan surface or swapchain?', 'No. The implementation and docs explicitly state that KMS owns presentation. Vulkan imports DMA BUF images, records rendering, and exports a sync file. There is no Vulkan display surface, Vulkan WSI swapchain, EGL import bridge, or GLES copy in this path.'),
        ('How is the GPU selected?', 'AshDrmRenderer::new takes the DRM major and minor device numbers. It enumerates physical devices and selects one whose primary or render DRM node matches. It then requires a graphics queue and verifies that FP16 scene format R16G16B16A16 SFLOAT can be a linearly filtered sampled image and a color attachment.'),
        ('Which Vulkan capabilities are mandatory?', 'The device must expose DRM format modifiers, external DMA BUF memory, foreign queue-family ownership, external memory FD, and external semaphore FD extensions. It must also support import and export of SYNC FD semaphores. Construction fails early with an error when any requirement is absent.'),
        ('Why is wgpu also present in the repository?', 'wgpu is used by a distinct Vulkan-only nested compositor path in wgpu_vulkan.rs. That path owns a window presentation surface and is intended for nested development. It is useful for validating Vulkan scene construction and buffer import without putting the active DRM session at risk, but it is not the production DRM compositor path.'),
        ('How are pipelines and descriptors organized?', 'The renderer creates a shared sampler, descriptor layouts for sampled images and samplers, and an output layout that additionally samples the color LUT. Pipelines are cached per output Vulkan format. For each format, it builds solid, textured, mesh, and output pipelines; viewport and scissor are dynamic.'),
        ('What blending model is used for client textures?', 'The texture and mesh pipelines use premultiplied alpha blending: source color ONE, destination color ONE_MINUS_SRC_ALPHA, with the corresponding alpha factors. This matches the compositor boundary, whose TextureQuad documentation identifies texture quads and tint as premultiplied.'),
    ]
    for question, answer in qas:
        content.append(para(question, 'Heading2', page_break=question == 'How is the GPU selected?'))
        content.append(para(answer))
    content.append(para('Rendering and memory questions', 'Heading1', page_break=True))
    qas = [
        ('What is the frame render sequence?', 'render first polls completed work, validates the target and scene inputs, imports or reuses the GBM target, ensures an FP16 retained scene, computes effective damage, records scene rendering, then runs an output pass into the scanout image. The command buffer transitions the target back to GENERAL and releases it to QUEUE_FAMILY_FOREIGN_EXT for KMS.'),
        ('How does retained damage work?', 'Each stream keeps bounded damage history, while every scanout target records initialization and the last stream frame it received. effective_damage_regions adds regions that an acquired target missed. Incomplete history or an uninitialized target safely implies broader repaint. Cached targets unused for more than 120 frames are retired.'),
        ('How are DMA BUFs imported?', 'The renderer imports GBM targets and eligible client buffers as Vulkan external-memory images using their DRM fourcc, modifier, plane offsets, and strides. Client texture cache compatibility includes dimensions, format, and modifier. The design notes state that RGB modifiers with auxiliary planes are supported when layouts share one DMA BUF memory object; disjoint memory and YUV formats remain rejected.'),
        ('When does the renderer upload instead of importing?', 'If a client buffer is not usable as an external import but synchronized pixel bytes are available, the renderer creates an owned image and uses staging buffers. In the wgpu nested path the analogous fallback is a mapped upload for linear buffers. A direct import failure without usable upload data results in the affected surface being skipped rather than sampling undefined data.'),
        ('How are texture resources retained and retired?', 'The renderer caches textures by a stable cache key and retains FrameRetention tokens in the PendingFrame until the completion fence signals. Replaced images, staging buffers, framebuffers, semaphores, command buffers, descriptor sets, and capture resources are destroyed only after poll observes completion. This prevents a client buffer or GPU resource from being reused too soon.'),
        ('How are color and HDR handled?', 'Client texture metadata carries transfer function, client-to-scene matrix, reference white, peak, and optional SDR inverse tone-mapping inputs. The raw path composes into FP16 linear scene space. The output pass applies an output matrix and 3D LUT; HDR10 operation prefers 10-bit GBM scanout, tone maps to BT.2020, encodes ST 2084 PQ, and relies on atomic KMS metadata. Failure to establish suitable buffers or signaling stays SDR.'),
        ('How are screenshots captured?', 'A requested capture adds an image-to-buffer copy before release to KMS. Pending capture data is retained with the submission and returned only after the completion fence. The code supports converting known 8-bit and packed 10-bit RGB layouts into RGBA8; unsupported layouts fail explicitly rather than continuing a capture loop.'),
    ]
    for question, answer in qas:
        content.append(para(question, 'Heading2'))
        content.append(para(answer))
    content.append(para('Synchronization and reliability questions', 'Heading1', page_break=True))
    qas = [
        ('How is producer synchronization handled for client DMA BUFs?', 'Queue-family ownership barriers do not wait for external writers. Before submission, the renderer deduplicates shared client DMA BUFs, exports each reservation-object read fence as a sync file, and imports it as a temporary binary semaphore. The graphics queue waits for those semaphores at FRAGMENT_SHADER before sampling.'),
        ('How is KMS notified that a frame is ready?', 'The renderer creates an exportable SYNC FD semaphore, signals it from the composition submission, and obtains an owned file descriptor from the external semaphore FD extension. The resulting AshDrmSubmission exposes that fence FD for atomic KMS IN_FENCE_FD handling.'),
        ('What happens after the renderer reads a client DMA BUF?', 'After a successful submit, the exported Vulkan release fence is duplicated and published back to each DMA BUF reservation object as a read fence. That protects other implicit-sync consumers. Wayland buffer retention separately prevents normal compositor-side reuse until GPU completion.'),
        ('How does the renderer recover completed work?', 'poll queries every completion fence. Completed PendingFrame resources are destroyed and captures are moved to a completed queue. The normal render path calls poll at its start, keeping resource release asynchronous instead of waiting for GPU idle for each frame.'),
        ('What failure behavior is designed for stalls or device loss?', 'The rendering design specifies bounded progress deadlines: a stalled GPU fence recreates the Vulkan device and scanout, and missing vblank rebuilds scanout. abandon_device marks a renderer so Drop avoids device_wait_idle, deliberately allowing process exit and kernel FD cleanup rather than risking an unbounded driver teardown.'),
        ('What are the main current limitations?', 'The repository describes raw Vulkan feature gaps including advanced GLES-effect parity and disjoint or YUV multi-plane DMA BUF import such as NV12 or P010. The VUI bridge may rasterize meshes into a cropped premultiplied overlay because native indexed mesh rendering is not yet reliable on supported DRM drivers. Treat HDR and zero-copy behavior as hardware and driver dependent.'),
    ]
    for question, answer in qas:
        content.append(para(question, 'Heading2'))
        content.append(para(answer))
    content.append(para('Engineering review prompts', 'Heading1'))
    content.append(table(['Review prompt', 'Code focused answer to verify'], [
        ('New external image path', 'Verify fourcc, modifier, plane count, offsets, strides, memory ownership, foreign queue-family barriers, acquire fence handling, and a safe upload or rejection fallback.'),
        ('New render pass', 'Verify retained target invalidation, damage propagation, image layouts, pipeline cache key, descriptor lifetime, FP16 scene compatibility, output encoding, and capture ordering.'),
        ('New async resource', 'Ensure it is held in PendingFrame or an equivalent fence-bound owner. Avoid CPU waits in the compositor hot path; release only after the relevant GPU fence signals.'),
        ('Driver failure path', 'Keep the existing bounded recovery behavior. Do not introduce teardown or presentation waits with no deadline. Preserve GLES as a viable recovery path.'),
    ]))
    content.append(para('Further engineering questions', 'Heading1'))
    qas = [
        ('When does the renderer deliberately repaint a full frame?', 'A full repaint is the safe outcome for a new or resized retained target, incomplete damage history, capture requirements, or normal full-redraw invalidation. This is an explicit correctness fallback rather than a failed optimization.'),
        ('How should damage performance be investigated?', 'Set FOCALDESK_DAMAGE_DEBUG to 1 or enable Log damage regions in Settings. The design notes say the renderer periodically logs input and compacted rectangle counts, damaged-area percentages, fallbacks, and cleanup metrics. FOCALDESK_RENDER_TIMINGS samples GPU-completion timing every 120 frames and intentionally serializes only sampled frames.'),
        ('What output changes can occur without reallocating scanout buffers?', 'The documented Auto, sRGB, and Display P3 gamut choices replace the output color transform and ICC LUT in place. They do not recreate GBM buffers, modeset a connector, or interrupt active HDR. Changes that alter topology, target dimensions, or scanout properties require the normal output rebuild path.'),
        ('What should be tested before merging renderer work?', 'Test the affected crate behavior and a nested path first. For DRM, KMS, DMA BUF, HDR, or synchronization changes, retain a recovery session and use the repository hardware regression coverage on representative drivers. Test error paths such as rejected import, stale damage, failed fence export, and device abandonment—not only successful rendering.'),
    ]
    for question, answer in qas:
        content.append(para(question, 'Heading2'))
        content.append(para(answer))
    body = ''.join(content) + '<w:sectPr><w:footerReference w:type="default" r:id="rId1"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1080" w:right="1080" w:bottom="1080" w:left="1080" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr>'
    styles = f'''<w:styles xmlns:w="{NS}"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Aptos" w:hAnsi="Aptos"/><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:pPr><w:keepLines/><w:spacing w:after="130" w:line="276" w:lineRule="auto"/></w:pPr></w:style>
<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:pPr><w:spacing w:after="160"/></w:pPr><w:rPr><w:b/><w:sz w:val="38"/><w:color w:val="000000"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Subtitle"><w:name w:val="Subtitle"/><w:pPr><w:spacing w:after="360"/></w:pPr><w:rPr><w:sz w:val="22"/><w:color w:val="404040"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:pPr><w:keepNext/><w:spacing w:before="300" w:after="150"/></w:pPr><w:rPr><w:b/><w:sz w:val="30"/><w:color w:val="000000"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:pPr><w:keepNext/><w:spacing w:before="200" w:after="90"/></w:pPr><w:rPr><w:b/><w:sz w:val="23"/><w:color w:val="000000"/></w:rPr></w:style></w:styles>'''
    document = f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{NS}" xmlns:r="{R}"><w:body>{body}</w:body></w:document>'
    content_types = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/></Types>'''
    root_rels = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>'''
    doc_rels = '''<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>'''
    footer = f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:ftr xmlns:w="{NS}"><w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:color w:val="666666"/><w:sz w:val="18"/></w:rPr><w:t>FocalDesk Vulkan Renderer Engineering Questions and Answers</w:t></w:r></w:p></w:ftr>'
    OUT.parent.mkdir(exist_ok=True)
    with ZipFile(OUT, 'w', ZIP_DEFLATED) as package:
        package.writestr('[Content_Types].xml', content_types)
        package.writestr('_rels/.rels', root_rels)
        package.writestr('word/document.xml', document)
        package.writestr('word/styles.xml', styles)
        package.writestr('word/footer1.xml', footer)
        package.writestr('word/_rels/document.xml.rels', doc_rels)
    print(OUT)

if __name__ == '__main__':
    main()
