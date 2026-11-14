package io.github.schrumbo.manycraft;

import com.mojang.blaze3d.platform.Window;
import net.minecraft.client.Minecraft;
import io.github.schrumbo.manycraft.mixin.OptionInstanceAccessor;
import net.minecraft.client.OptionInstance;
import net.minecraft.client.Options;
import org.lwjgl.sdl.SDLVideo;
import java.nio.IntBuffer;
import org.lwjgl.system.JNI;
import org.lwjgl.system.MemoryStack;
import org.lwjgl.system.Platform;
import org.lwjgl.system.windows.WindowsLibrary;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class GameWindow {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	private static final long ALLOW_SET_FOREGROUND_WINDOW = lookup();
	private static final int PATIENCE_TICKS = 10;
	private static Runnable pending;
	private static int pendingWidth;
	private static int pendingHeight;
	private static boolean drawn;
	private static int waited;
	private static Boolean chosen;

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

	static void show(Minecraft mc, int x, int y, int w, int h, String mode, Runnable shown) {
		pending = null;
		Window window = mc.getWindow();
		long handle = window.handle();
		OptionInstance<Boolean> fullscreen = mc.options.fullscreen();
		if (chosen == null) {
			chosen = fullscreen.get();
		}
		boolean full = "fullscreen".equals(mode);
		if (fullscreen.get() && !full) {
			fullscreen.set(false);
			window.updateFullscreenIfChanged();
		}
		Runnable reveal = () -> {
			SDLVideo.SDL_ShowWindow(handle);
			if ("maximized".equals(mode)) {
				SDLVideo.SDL_MaximizeWindow(handle);
			}
			if (full && !fullscreen.get()) {
				fullscreen.set(true);
				window.updateFullscreenIfChanged();
			}
			SDLVideo.SDL_RaiseWindow(handle);
			shown.run();
		};
		if (!fullscreen.get()) {
			if ((SDLVideo.SDL_GetWindowFlags(handle) & SDLVideo.SDL_WINDOW_MAXIMIZED) != 0) {
				SDLVideo.SDL_RestoreWindow(handle);
			}
			int width = window.getWidth();
			int height = window.getHeight();
			SDLVideo.SDL_SetWindowSize(handle, Math.max(w, 320), Math.max(h, 240));
			SDLVideo.SDL_SetWindowPosition(handle, x, y);
			if (!shown(mc) && (width != window.getWidth() || height != window.getHeight() || resizes(handle, width, height))) {
				pending = reveal;
				pendingWidth = width;
				pendingHeight = height;
				drawn = false;
				waited = 0;
				return;
			}
		}
		reveal.run();
	}

	static void saving(Options options, Runnable save) {
		Boolean own = chosen;
		OptionInstance<Boolean> fullscreen = options.fullscreen();
		Boolean now = fullscreen.get();
		if (own == null || own.equals(now)) {
			save.run();
			return;
		}
		OptionInstanceAccessor raw = (OptionInstanceAccessor) (Object) fullscreen;
		raw.manycraft$setValue(own);
		try {
			save.run();
		} finally {
			raw.manycraft$setValue(now);
		}
	}

	static boolean revealing() {
		return pending != null;
	}

	static void cancel() {
		pending = null;
	}

	static void frame(Minecraft mc) {
		Runnable reveal = pending;
		if (reveal == null) {
			return;
		}
		if (drawn) {
			pending = null;
			reveal.run();
		} else if (mc.getWindow().getWidth() != pendingWidth || mc.getWindow().getHeight() != pendingHeight) {
			drawn = true;
		}
	}

	static void tick() {
		Runnable reveal = pending;
		if (reveal != null && ++waited >= PATIENCE_TICKS) {
			pending = null;
			reveal.run();
		}
	}

	private static boolean resizes(long handle, int width, int height) {
		try (MemoryStack stack = MemoryStack.stackPush()) {
			IntBuffer w = stack.mallocInt(1);
			IntBuffer h = stack.mallocInt(1);
			SDLVideo.SDL_GetWindowSizeInPixels(handle, w, h);
			return w.get(0) != width || h.get(0) != height;
		}
	}
}
