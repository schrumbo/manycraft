package io.github.schrumbo.manycraft.agent.payload;

import com.mojang.blaze3d.pipeline.RenderTarget;
import io.github.schrumbo.manycraft.agent.Agent;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import net.minecraft.client.Minecraft;
import net.minecraft.client.OptionInstance;
import net.minecraft.client.Options;
import net.minecraft.client.gui.Gui;
import net.minecraft.client.gui.screens.Screen;
import net.minecraft.client.input.KeyEvent;
import net.minecraft.client.renderer.GameRenderer;
import net.minecraft.client.renderer.LevelRenderer;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class Compat {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	private static final boolean GUI_SCREEN = has(Gui.class, "screen");
	private static final Field SCREEN = GUI_SCREEN ? null : Access.field(Minecraft.class, "screen");
	private static final boolean SHORTCUT_KEY = has(KeyEvent.class, "shortcutKey");
	private static final boolean HUD = field(Gui.class, "hud");
	private static final Field HIDE_GUI = HUD ? null : Access.field(Options.class, "hideGui");
	private static final boolean LEVEL_EXTRACTOR = field(Minecraft.class, "levelExtractor");
	private static final Method LEVEL_RENDERER_CHANGED = LEVEL_EXTRACTOR ? null : method(LevelRenderer.class, "allChanged");
	private static final boolean RENDERER_TARGET = has(GameRenderer.class, "mainRenderTarget");
	private static final Method MINECRAFT_TARGET = RENDERER_TARGET ? null : method(Minecraft.class, "mainRenderTarget", "getMainRenderTarget");
	private static final Method MIN_INCLUSIVE = range("minInclusive");
	private static final Method MAX_INCLUSIVE = range("maxInclusive");

	private Compat() {}

	static Screen screen(Minecraft mc) {
		return GUI_SCREEN ? mc.gui.screen() : Access.get(SCREEN, mc);
	}

	static int shortcutKey(KeyEvent event) {
		return SHORTCUT_KEY ? event.shortcutKey() : event.key();
	}

	static boolean hudHidden(Minecraft mc) {
		return HUD ? mc.gui.hud.isHidden() : Boolean.TRUE.equals(Access.get(HIDE_GUI, mc.options));
	}

	static void allChanged(Minecraft mc) {
		if (LEVEL_EXTRACTOR) {
			mc.levelExtractor.allChanged();
		} else {
			invoke(LEVEL_RENDERER_CHANGED, mc.levelRenderer);
		}
	}

	static RenderTarget mainTarget(Minecraft mc) {
		if (RENDERER_TARGET) {
			return mc.gameRenderer.mainRenderTarget();
		}
		return (RenderTarget) invoke(MINECRAFT_TARGET, mc);
	}

	/**
	 * The option's integer range as {min, max}, or null when the game hides it.
	 */
	static int[] range(OptionInstance<Integer> option) {
		Object values = option.values();
		Object min = invoke(MIN_INCLUSIVE, values);
		Object max = invoke(MAX_INCLUSIVE, values);
		return min instanceof Integer lo && max instanceof Integer hi ? new int[] {lo, hi} : null;
	}

	private static Object invoke(Method method, Object target) {
		if (method == null || target == null || !method.getDeclaringClass().isInstance(target)) {
			return null;
		}
		try {
			return method.invoke(target);
		} catch (ReflectiveOperationException | RuntimeException e) {
			return null;
		}
	}

	private static boolean has(Class<?> owner, String name) {
		for (Method m : owner.getMethods()) {
			if (m.getName().equals(name)) {
				return true;
			}
		}
		return false;
	}

	private static boolean field(Class<?> owner, String name) {
		try {
			owner.getField(name);
			return true;
		} catch (NoSuchFieldException e) {
			return false;
		}
	}

	private static Method method(Class<?> owner, String... names) {
		for (String name : names) {
			try {
				return owner.getMethod(name);
			} catch (NoSuchMethodException e) {
				continue;
			}
		}
		LOGGER.warn("{} has none of {}, the companion leaves it alone", owner.getSimpleName(), String.join(", ", names));
		return null;
	}

	private static Method range(String name) {
		try {
			Class<?> base = Class.forName("net.minecraft.client.OptionInstance$IntRangeBase", false, Compat.class.getClassLoader());
			Method m = base.getMethod(name);
			Agent.open(base, Compat.class.getModule());
			m.setAccessible(true);
			return m;
		} catch (ReflectiveOperationException | RuntimeException | LinkageError e) {
			LOGGER.warn("option ranges not found, background values stay unclamped: {}", e.toString());
			return null;
		}
	}
}
