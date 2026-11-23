# Manycraft changes to freya-winit

- A window resized to zero (minimized) frees the GPU resource cache and Skia's global resource cache (`Driver::release_caches`, called from `Window::apply_pending_resize`). The next frame after the window returns rebuilds what it draws.
