# Manycraft changes to freya-winit

- A window resized to zero (minimized) or reported occluded (hidden on macOS) frees the GPU resource cache and Skia's global resource cache (`Driver::release_caches`, called from `Window::apply_pending_resize` and on `WindowEvent::Occluded(true)`). The next frame after the window returns rebuilds what it draws.
