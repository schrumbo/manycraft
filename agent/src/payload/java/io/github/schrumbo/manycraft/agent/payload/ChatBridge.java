package io.github.schrumbo.manycraft.agent.payload;

import com.mojang.authlib.GameProfile;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;
import java.util.Optional;
import java.util.function.Consumer;
import net.minecraft.network.chat.ChatType;
import net.minecraft.network.chat.Component;
import net.minecraft.network.chat.PlayerChatMessage;
import net.minecraft.network.chat.Style;
import net.minecraft.network.chat.TextColor;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class ChatBridge {
	private static final String MARKER = "\u0000";
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	private static final int MAX_RUNS = 200;

	private final Consumer<String> sink;
	private final QueueDetector queue;

	ChatBridge(Consumer<String> sink, QueueDetector queue) {
		this.sink = sink;
		this.queue = queue;
	}

	void onGame(Component message, boolean overlay) {
		try {
			String text = message.getString();
			queue.onSystemMessage(text, System.currentTimeMillis());
			if (!overlay) {
				sink.accept(Protocol.chat("system", null, text, runs(message)));
			}
		} catch (RuntimeException e) {
			LOGGER.warn("chat bridge failed", e);
		}
	}

	static List<Protocol.Run> runs(Component message) {
		List<Protocol.Run> out = new ArrayList<>();
		message.<Void>visit((Style style, String text) -> {
			if (!text.isEmpty() && out.size() < MAX_RUNS) {
				TextColor color = style.getColor();
				out.add(new Protocol.Run(text, color == null ? null : color.getValue(), style.isBold(), style.isItalic(), style.isUnderlined(), style.isStrikethrough()));
			}
			return Optional.empty();
		}, Style.EMPTY);
		return out;
	}

	static String stripDecoration(String decorated, String template) {
		int marker = template.indexOf(MARKER);
		if (marker < 0) {
			return decorated;
		}
		String prefix = template.substring(0, marker);
		String suffix = template.substring(marker + 1);
		if (decorated.length() >= prefix.length() + suffix.length() && decorated.startsWith(prefix) && decorated.endsWith(suffix)) {
			return decorated.substring(prefix.length(), decorated.length() - suffix.length());
		}
		return decorated;
	}

	private static String plainContent(String decorated, ChatType.Bound bound) {
		return stripDecoration(decorated, bound.decorate(Component.literal(MARKER)).getString());
	}

	void onChat(Component message, PlayerChatMessage signed, GameProfile sender, ChatType.Bound bound, Instant receivedAt) {
		try {
			var key = bound.chatType().unwrapKey().orElse(null);
			if (key == null || key.equals(ChatType.MSG_COMMAND_OUTGOING) || key.equals(ChatType.TEAM_MSG_COMMAND_OUTGOING)) {
				return;
			}
			String from = bound.name().getString();
			String text = signed != null ? signed.signedContent() : plainContent(message.getString(), bound);
			sink.accept(Protocol.chat("player", from.isEmpty() ? null : from, text, runs(bound.decorate(message))));
		} catch (RuntimeException e) {
			LOGGER.warn("chat bridge failed", e);
		}
	}
}
