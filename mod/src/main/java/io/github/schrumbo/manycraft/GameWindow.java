package io.github.schrumbo.manycraft;

import com.mojang.blaze3d.platform.Window;
import net.minecraft.client.Minecraft;
import net.minecraft.client.OptionInstance;
import org.lwjgl.sdl.SDLVideo;
import org.lwjgl.system.JNI;
import org.lwjgl.system.Platform;
import org.lwjgl.system.windows.WindowsLibrary;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class GameWindow {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	private static final long ALLOW_SET_FOREGROUND_WINDOW = lookup();

	private GameWindow() {}

	private static long lookup() {
		if (Platform.get() != Platform.WINDOWS) {
			return 0;
		}
		try {
			return new WindowsLibrary("user32").getFunctionAddress("AllowSetForegroundWindow");
		} catch (RuntimeException | LinkageError e) {
			LOGGER.warn("no foreground handoff: {}", e.getMessage());
			return 0;
		}
	}

	static void grantForeground() {
		if (ALLOW_SET_FOREGROUND_WINDOW != 0) {
			JNI.invokeI(-1, ALLOW_SET_FOREGROUND_WINDOW);
		}
	}

	static boolean shown(Minecraft mc) {
		return (SDLVideo.SDL_GetWindowFlags(mc.getWindow().handle()) & SDLVideo.SDL_WINDOW_HIDDEN) == 0;
	}

	static void show(Minecraft mc, int x, int y, int w, int h, String mode) {
		Window window = mc.getWindow();
		long handle = window.handle();
		OptionInstance<Boolean> fullscreen = mc.options.fullscreen();
		boolean full = "fullscreen".equals(mode);
		if (fullscreen.get() && !full) {
			fullscreen.set(false);
			window.updateFullscreenIfChanged();
		}
		if (!fullscreen.get()) {
			if ((SDLVideo.SDL_GetWindowFlags(handle) & SDLVideo.SDL_WINDOW_MAXIMIZED) != 0) {
				SDLVideo.SDL_RestoreWindow(handle);
			}
			SDLVideo.SDL_SetWindowSize(handle, Math.max(w, 320), Math.max(h, 240));
			SDLVideo.SDL_SetWindowPosition(handle, x, y);
		}
		SDLVideo.SDL_ShowWindow(handle);
		if ("maximized".equals(mode)) {
			SDLVideo.SDL_MaximizeWindow(handle);
		}
		if (full && !fullscreen.get()) {
			fullscreen.set(true);
			window.updateFullscreenIfChanged();
		}
		SDLVideo.SDL_RaiseWindow(handle);
	}
}
