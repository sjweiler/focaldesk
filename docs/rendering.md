

# Rendering Pipeline

FocalDesk uses a GPU-accelerated rendering pipeline built on OpenGL ES.

The renderer is responsible for composing application surfaces, shell UI, shaders, cursors, and desktop effects into the final image presented through DRM/KMS or other backends.

Rendering is designed to be modular so additional effects, color management, HDR, and future rendering backends can be added without redesigning the compositor.
