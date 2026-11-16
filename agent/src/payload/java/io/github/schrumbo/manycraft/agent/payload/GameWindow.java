package io.github.schrumbo.manycraft.agent.payload;

import com.mojang.blaze3d.platform.Window;
import java.lang.reflect.Field;
import net.minecraft.client.Minecraft;
import net.minecraft.client.OptionInstance;
import net.minecraft.client.Options;
import org.lwjgl.system.JNI;
import org.lwjgl.system.Platform;
import org.lwjgl.system.windows.User32;
import org.lwjgl.system.windows.WindowsLibrary;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class GameWindow {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	private static final long ALLOW_SET_FOREGROUND_WINDOW = lookup();
	private static final Field OPTION_VALUE = Access.field(OptionInstance.class, "value");
	private static NativeWindow nativeWindow;
	private static final int PATIENCE_TICKS = 10;
	private static Runnable pending;
	private static int pendingWidth;
	private static int pendingHeight;
	private static boolean drawn;
	private static int waited;
	private static Boolean chosen;
	private static int hiddenWidth;
	private static int hiddenHeight;

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

	static NativeWindow of(Minecraft mc) {
		long handle = mc.getWindow().handle();
		NativeWindow w = nativeWindow;
		if (w == null || w.handle() != handle) {
			w = NativeWindow.of(handle);
			nativeWindow = w;
		}
		return w;
	}

	static boolean shown(Minecraft mc) {
		return !of(mc).hidden();
	}

	static void show(Minecraft mc, int x, int y, int w, int h, String mode, Runnable shown) {
		pending = null;
		Window window = mc.getWindow();
		NativeWindow handle = of(mc);
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
			handle.show();
			if ("maximized".equals(mode)) {
				handle.maximize();
			}
			if (full && !fullscreen.get()) {
				fullscreen.set(true);
				window.updateFullscreenIfChanged();
			}
			handle.raise();
			shown.run();
		};
		if (!fullscreen.get()) {
			WindowSteps steps = WindowSteps.of(!shown(mc), handle.maximized(), mode);
			if (steps.unzoom()) {
				unzoom(handle);
			}
			if (steps.restore()) {
				handle.restore();
			}
			int width = window.getWidth();
			int height = window.getHeight();
			if (hiddenWidth == 0 && !shown(mc)) {
				remember(handle);
			}
			handle.setSize(Math.max(w, 320), Math.max(h, 240));
			handle.setPosition(x, y);
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

	static void shrink(Minecraft mc) {
		NativeWindow handle = of(mc);
		if (hiddenWidth == 0 || handle.fullscreen() || handle.maximized()) {
			return;
		}
		handle.setSize(hiddenWidth, hiddenHeight);
	}

	private static void remember(NativeWindow handle) {
		int[] size = handle.size();
		if (size != null) {
			hiddenWidth = size[0];
			hiddenHeight = size[1];
		}
	}

	static void saving(Options options, Runnable save) {
		Boolean own = chosen;
		OptionInstance<Boolean> fullscreen = options.fullscreen();
		Boolean now = fullscreen.get();
		if (own == null || own.equals(now)) {
			save.run();
			return;
		}
		if (!Access.set(OPTION_VALUE, fullscreen, own)) {
			save.run();
			return;
		}
		try {
			save.run();
		} finally {
			Access.set(OPTION_VALUE, fullscreen, now);
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

	private static void unzoom(NativeWindow handle) {
		if (Platform.get() != Platform.WINDOWS) {
			return;
		}
		long hwnd = handle.hwnd();
		if (hwnd == 0) {
			return;
		}
		long style = User32.GetWindowLongPtr(null, hwnd, User32.GWL_STYLE);
		User32.SetWindowLongPtr(null, hwnd, User32.GWL_STYLE, style & ~(long) User32.WS_MAXIMIZE);
		User32.SetWindowPos(null, hwnd, 0, 0, 0, 0, 0, User32.SWP_NOMOVE | User32.SWP_NOSIZE | User32.SWP_NOZORDER | User32.SWP_NOACTIVATE | User32.SWP_FRAMECHANGED);
	}

	private static boolean resizes(NativeWindow handle, int width, int height) {
		int[] pixels = handle.pixelSize();
		return pixels[0] != width || pixels[1] != height;
	}
}
