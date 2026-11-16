package io.github.schrumbo.manycraft.agent.payload;

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
	private static final long HINT_NANOS = 8_000_000_000L;

	static final Identifier BADGE = Identifier.fromNamespaceAndPath("manycraft", "badge");

	private volatile Protocol.Incoming.Identity identity;
	private volatile boolean badge = true;
	private volatile String hint;
	private volatile long hintUntil;

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

	void showHint(String text) {
		hintUntil = System.nanoTime() + HINT_NANOS;
		hint = text;
	}

	void extractBadge(GuiGraphicsExtractor g, DeltaTracker delta) {
		Protocol.Incoming.Identity id = identity;
		Minecraft mc = Minecraft.getInstance();
		if (Compat.hudHidden(mc) || mc.getDebugOverlay().showDebugScreen()) {
			return;
		}
		Font font = mc.font;
		int h = font.lineHeight + PAD * 2 - 1;
		int y = MARGIN;
		if (badge && id != null) {
			int w = PAD + CHIP + GAP + font.width(id.name()) + PAD;
			int chipY = y + (h - CHIP) / 2;
			g.fill(MARGIN, y, MARGIN + w, y + h, BACKDROP);
			g.fill(MARGIN + PAD, chipY, MARGIN + PAD + CHIP, chipY + CHIP, 0xFF000000 | id.rgb());
			g.text(font, id.name(), MARGIN + PAD + CHIP + GAP, y + PAD, TEXT, false);
			y += h + GAP;
		}
		String line = hint;
		if (line == null) {
			return;
		}
		if (System.nanoTime() - hintUntil > 0) {
			hint = null;
			return;
		}
		g.fill(MARGIN, y, MARGIN + PAD + font.width(line) + PAD, y + h, BACKDROP);
		g.text(font, line, MARGIN + PAD, y + PAD, TEXT, false);
	}

	private static void setIcon(Minecraft mc, String path) {
		if (path == null || !Files.isRegularFile(Path.of(path))) {
			return;
		}
		try (InputStream in = Files.newInputStream(Path.of(path)); NativeImage image = NativeImage.read(in)) {
			GameWindow.of(mc).setIcon(image);
		} catch (IOException | RuntimeException | LinkageError e) {
			LOGGER.warn("window icon not set: {}", e.toString());
		}
	}
}
