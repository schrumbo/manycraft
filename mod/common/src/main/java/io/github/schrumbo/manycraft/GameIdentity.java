package io.github.schrumbo.manycraft;

import com.mojang.blaze3d.platform.NativeImage;
import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import net.minecraft.client.DeltaTracker;
import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.Font;
import net.minecraft.client.gui.GuiGraphicsExtractor;
import net.minecraft.resources.Identifier;
import org.lwjgl.sdl.SDLPixels;
import org.lwjgl.sdl.SDLSurface;
import org.lwjgl.sdl.SDLVideo;
import org.lwjgl.sdl.SDL_Surface;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class GameIdentity {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	private static final int MARGIN = 4;
	private static final int PAD = 3;
	private static final int CHIP = 7;
	private static final int GAP = 4;
	private static final int BACKDROP = 0x80000000;
	private static final int TEXT = 0xE6FFFFFF;

	static final Identifier BADGE = Identifier.fromNamespaceAndPath("manycraft", "badge");

	private volatile Protocol.Incoming.Identity identity;
	private volatile boolean badge = true;

	static String title(String name, String batch) {
		return name + " · " + batch;
	}

	String title() {
		Protocol.Incoming.Identity id = identity;
		return id == null ? null : title(id.name(), id.batch());
	}

	void apply(Minecraft mc, Protocol.Incoming.Identity next) {
		if (next.equals(identity)) {
			return;
		}
		identity = next;
		mc.updateTitle();
		setIcon(mc, next.icon());
	}

	void showBadge(boolean visible) {
		badge = visible;
	}

	void extractBadge(GuiGraphicsExtractor g, DeltaTracker delta) {
		Protocol.Incoming.Identity id = identity;
		Minecraft mc = Minecraft.getInstance();
		if (!badge || id == null || mc.gui.hud.isHidden() || mc.getDebugOverlay().showDebugScreen()) {
			return;
		}
		Font font = mc.font;
		int h = font.lineHeight + PAD * 2 - 1;
		int w = PAD + CHIP + GAP + font.width(id.name()) + PAD;
		int chipY = MARGIN + (h - CHIP) / 2;
		g.fill(MARGIN, MARGIN, MARGIN + w, MARGIN + h, BACKDROP);
		g.fill(MARGIN + PAD, chipY, MARGIN + PAD + CHIP, chipY + CHIP, 0xFF000000 | id.rgb());
		g.text(font, id.name(), MARGIN + PAD + CHIP + GAP, MARGIN + PAD, TEXT, false);
	}

	private static void setIcon(Minecraft mc, String path) {
		if (path == null || !Files.isRegularFile(Path.of(path))) {
			return;
		}
		try (InputStream in = Files.newInputStream(Path.of(path)); NativeImage image = NativeImage.read(in)) {
			SDL_Surface surface = SDLSurface.SDL_CreateSurfaceFrom(image.getWidth(), image.getHeight(), SDLPixels.SDL_PIXELFORMAT_ABGR8888, image.getPixelBytes(), image.getWidth() * 4);
			if (surface == null) {
				return;
			}
			try {
				SDLVideo.SDL_SetWindowIcon(mc.getWindow().handle(), surface);
			} finally {
				SDLSurface.SDL_DestroySurface(surface);
			}
		} catch (IOException | RuntimeException e) {
			LOGGER.warn("window icon not set: {}", e.toString());
		}
	}
}
