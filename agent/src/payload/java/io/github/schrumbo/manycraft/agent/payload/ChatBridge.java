package io.github.schrumbo.manycraft.agent.payload;

import com.mojang.authlib.GameProfile;
import java.time.Instant;
import java.util.function.Consumer;
import net.minecraft.client.multiplayer.chat.ChatListener;
import net.minecraft.network.chat.ChatType;
import net.minecraft.network.chat.Component;
import net.minecraft.network.chat.PlayerChatMessage;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class ChatBridge {
	private static final String MARKER = "\u0000";
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	private static final boolean OVERLAY_APART = overlayApart();

	private final Consumer<String> sink;
	private final QueueDetector queue;

	ChatBridge(Consumer<String> sink, QueueDetector queue) {
		this.sink = sink;
		this.queue = queue;
	}

	void onGame(Component message, boolean flag) {
		boolean overlay = flag && !OVERLAY_APART;
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

	void onOverlay(Component message) {
		try {
			queue.onSystemMessage(message.getString(), System.currentTimeMillis());
		} catch (RuntimeException e) {
			LOGGER.warn("chat bridge failed", e);
		}
	}

	private static boolean overlayApart() {
		try {
			ChatListener.class.getMethod("handleOverlay", Component.class);
			return true;
		} catch (NoSuchMethodException e) {
			return false;
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

	private static boolean outgoing(ChatType.Bound bound) {
		var key = bound.chatType().unwrapKey().orElse(null);
		return key == null || key.equals(ChatType.MSG_COMMAND_OUTGOING) || key.equals(ChatType.TEAM_MSG_COMMAND_OUTGOING);
	}

	private static boolean whisper(ChatType.Bound bound) {
		var key = bound.chatType().unwrapKey().orElse(null);
		return key != null && (key.equals(ChatType.MSG_COMMAND_INCOMING) || key.equals(ChatType.TEAM_MSG_COMMAND_INCOMING));
	}

	private static String plainContent(String decorated, ChatType.Bound bound) {
		return stripDecoration(decorated, bound.decorate(Component.literal(MARKER)).getString());
	}

	void onDisguised(Component message, ChatType.Bound bound) {
		try {
			if (outgoing(bound)) {
				return;
			}
			String from = bound.name().getString();
			sink.accept(Protocol.chat(whisper(bound) ? "whisper" : "player", from.isEmpty() ? null : from, message.getString()));
		} catch (RuntimeException e) {
			LOGGER.warn("chat bridge failed", e);
		}
	}

	void onChat(Component message, PlayerChatMessage signed, GameProfile sender, ChatType.Bound bound, Instant receivedAt) {
		try {
			if (outgoing(bound)) {
				return;
			}
			String from = bound.name().getString();
			boolean whisper = whisper(bound);
			String text = signed != null ? signed.signedContent() : plainContent(message.getString(), bound);
			sink.accept(Protocol.chat(whisper ? "whisper" : "player", from.isEmpty() ? null : from, text));
		} catch (RuntimeException e) {
			LOGGER.warn("chat bridge failed", e);
		}
	}
}
