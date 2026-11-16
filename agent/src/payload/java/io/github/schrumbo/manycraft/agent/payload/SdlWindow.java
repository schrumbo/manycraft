package io.github.schrumbo.manycraft.agent.payload;

import com.mojang.blaze3d.platform.NativeImage;
import java.nio.IntBuffer;
import org.lwjgl.sdl.SDLPixels;
import org.lwjgl.sdl.SDLProperties;
import org.lwjgl.sdl.SDLSurface;
import org.lwjgl.sdl.SDLVideo;
import org.lwjgl.sdl.SDL_Surface;
import org.lwjgl.system.MemoryStack;

record SdlWindow(long handle) implements NativeWindow {
	@Override
	public void show() {
		SDLVideo.SDL_ShowWindow(handle);
	}

	@Override
	public void hide() {
		SDLVideo.SDL_HideWindow(handle);
	}

	@Override
	public void maximize() {
		SDLVideo.SDL_MaximizeWindow(handle);
	}

	@Override
	public void restore() {
		SDLVideo.SDL_RestoreWindow(handle);
	}

	@Override
	public void raise() {
		SDLVideo.SDL_RaiseWindow(handle);
	}

	@Override
	public void keepOpen() {}

	@Override
	public void setSize(int w, int h) {
		SDLVideo.SDL_SetWindowSize(handle, w, h);
	}

	@Override
	public void setPosition(int x, int y) {
		SDLVideo.SDL_SetWindowPosition(handle, x, y);
	}

	@Override
	public int[] size() {
		try (MemoryStack stack = MemoryStack.stackPush()) {
			IntBuffer w = stack.mallocInt(1);
			IntBuffer h = stack.mallocInt(1);
			return SDLVideo.SDL_GetWindowSize(handle, w, h) ? new int[] {w.get(0), h.get(0)} : null;
		}
	}

	@Override
	public int[] pixelSize() {
		try (MemoryStack stack = MemoryStack.stackPush()) {
			IntBuffer w = stack.mallocInt(1);
			IntBuffer h = stack.mallocInt(1);
			SDLVideo.SDL_GetWindowSizeInPixels(handle, w, h);
			return new int[] {w.get(0), h.get(0)};
		}
	}

	@Override
	public boolean hidden() {
		return (SDLVideo.SDL_GetWindowFlags(handle) & SDLVideo.SDL_WINDOW_HIDDEN) != 0;
	}

	@Override
	public boolean maximized() {
		return (SDLVideo.SDL_GetWindowFlags(handle) & SDLVideo.SDL_WINDOW_MAXIMIZED) != 0;
	}

	@Override
	public boolean fullscreen() {
		return (SDLVideo.SDL_GetWindowFlags(handle) & SDLVideo.SDL_WINDOW_FULLSCREEN) != 0;
	}

	@Override
	public long hwnd() {
		return SDLProperties.SDL_GetPointerProperty(SDLVideo.SDL_GetWindowProperties(handle), SDLVideo.SDL_PROP_WINDOW_WIN32_HWND_POINTER, 0);
	}

	@Override
	public void setIcon(NativeImage image) {
		SDL_Surface surface = SDLSurface.SDL_CreateSurfaceFrom(image.getWidth(), image.getHeight(), SDLPixels.SDL_PIXELFORMAT_ABGR8888, image.getPixelBytes(), image.getWidth() * 4);
		if (surface == null) {
			return;
		}
		try {
			SDLVideo.SDL_SetWindowIcon(handle, surface);
		} finally {
			SDLSurface.SDL_DestroySurface(surface);
		}
	}
}
