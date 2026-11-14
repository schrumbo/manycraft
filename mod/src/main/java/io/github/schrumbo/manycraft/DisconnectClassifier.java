package io.github.schrumbo.manycraft;

import java.util.List;
import java.util.Locale;
import java.util.regex.Pattern;
import net.minecraft.network.chat.Component;
import net.minecraft.network.chat.contents.TranslatableContents;

final class DisconnectClassifier {
	private static final Pattern BANNED = Pattern.compile("(?<![a-z])(banned|blacklisted|ban)(?![a-z])");
	private static final List<String> WHITELIST = List.of("whitelist", "white-list", "white list");
	private static final List<String> FULL = List.of("server is full", "server full", "is full");
	private static final List<String> AUTH = List.of("failed to verify username", "invalid session", "failed to log in", "login failed", "session expired", "public key");
	private static final List<String> KICKED = List.of("kicked", "idle", "afk", "flying", "another location", "already online", "already connected", "spam");
	private static final List<String> DOWN = List.of("connection refused", "timed out", "timeout", "connection lost", "connection closed", "connection reset", "server closed", "shutting down", "restarting", "restart", "stopping", "no route", "unknown host", "unresolved", "unreachable", "end of stream", "authentication servers are down");

	private DisconnectClassifier() {}

	static String classify(Component reason) {
		List<String> keys = new java.util.ArrayList<>();
		collect(reason, keys);
		return classify(keys, reason.getString());
	}

	static String classify(List<String> keys, String text) {
		for (String key : keys) {
			String bucket = byKey(key);
			if (bucket != null) {
				return bucket;
			}
		}
		return byText(text == null ? "" : text.toLowerCase(Locale.ROOT));
	}

	private static void collect(Component component, List<String> keys) {
		if (component.getContents() instanceof TranslatableContents translatable) {
			keys.add(translatable.getKey());
			for (Object arg : translatable.getArgs()) {
				if (arg instanceof Component nested) {
					collect(nested, keys);
				}
			}
		}
		for (Component sibling : component.getSiblings()) {
			collect(sibling, keys);
		}
	}

	private static String byKey(String key) {
		if (key.startsWith("multiplayer.disconnect.banned")) {
			return "banned";
		}
		if (key.equals("multiplayer.disconnect.not_whitelisted")) {
			return "whitelist";
		}
		if (key.equals("multiplayer.disconnect.server_full")) {
			return "server_full";
		}
		if (key.equals("multiplayer.disconnect.authservers_down") || key.equals("disconnect.loginFailedInfo.serversUnavailable")) {
			return "server_down";
		}
		if (key.startsWith("disconnect.loginFailed") || key.equals("multiplayer.disconnect.unverified_username") || key.contains("public_key")) {
			return "auth";
		}
		return switch (key) {
			case "multiplayer.disconnect.kicked", "multiplayer.disconnect.idling", "multiplayer.disconnect.flying", "multiplayer.disconnect.duplicate_login", "multiplayer.disconnect.name_taken", "disconnect.kicked" -> "kicked";
			case "multiplayer.disconnect.server_shutdown", "multiplayer.disconnect.slow_login", "disconnect.timeout", "disconnect.lost", "disconnect.closed", "disconnect.endOfStream" -> "server_down";
			default -> null;
		};
	}

	private static String byText(String text) {
		if (BANNED.matcher(text).find()) {
			return "banned";
		}
		if (containsAny(text, WHITELIST)) {
			return "whitelist";
		}
		if (containsAny(text, FULL)) {
			return "server_full";
		}
		if (containsAny(text, AUTH)) {
			return "auth";
		}
		if (containsAny(text, KICKED)) {
			return "kicked";
		}
		if (containsAny(text, DOWN)) {
			return "server_down";
		}
		return "unknown";
	}

	private static boolean containsAny(String text, List<String> needles) {
		for (String needle : needles) {
			if (text.contains(needle)) {
				return true;
			}
		}
		return false;
	}
}
