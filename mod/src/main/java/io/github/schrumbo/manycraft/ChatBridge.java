package io.github.schrumbo.manycraft;

import com.mojang.authlib.GameProfile;
import java.time.Instant;
import java.util.function.Consumer;
import net.fabricmc.fabric.api.client.message.v1.ClientReceiveMessageEvents;
import net.minecraft.network.chat.ChatType;
import net.minecraft.network.chat.Component;
import net.minecraft.network.chat.PlayerChatMessage;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class ChatBridge {
	private static final String MARKER = "\u0000";
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");

	private final Consumer<String> sink;
	private final QueueDetector queue;

	ChatBridge(Consumer<String> sink, QueueDetector queue) {
		this.sink = sink;
		this.queue = queue;
	}

	void register() {
		ClientReceiveMessageEvents.GAME.register(this::onGame);
		ClientReceiveMessageEvents.CHAT.register(this::onChat);
	}

	private void onGame(Component message, boolean overlay) {
		try {
			String text = message.getString();
			queue.onSystemMessage(text, System.currentTimeMillis());
			if (!overlay) {
				sink.accept(Protocol.chat("system", null, text));
			}
		} catch (RuntimeException e) {
			LOGGER.warn("chat bridge failed", e);
		}
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

	private void onChat(Component message, PlayerChatMessage signed, GameProfile sender, ChatType.Bound bound, Instant receivedAt) {
		try {
			var key = bound.chatType().unwrapKey().orElse(null);
			if (key == null || key.equals(ChatType.MSG_COMMAND_OUTGOING) || key.equals(ChatType.TEAM_MSG_COMMAND_OUTGOING)) {
				return;
			}
			String from = bound.name().getString();
			boolean whisper = key.equals(ChatType.MSG_COMMAND_INCOMING) || key.equals(ChatType.TEAM_MSG_COMMAND_INCOMING);
			String text = signed != null ? signed.signedContent() : plainContent(message.getString(), bound);
			sink.accept(Protocol.chat(whisper ? "whisper" : "player", from.isEmpty() ? null : from, text));
		} catch (RuntimeException e) {
			LOGGER.warn("chat bridge failed", e);
		}
	}
}
