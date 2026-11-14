package io.github.schrumbo.manycraft;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import com.google.gson.JsonPrimitive;
import java.util.Optional;

final class Protocol {
	private static final Gson GSON = new GsonBuilder().serializeNulls().disableHtmlEscaping().create();

	sealed interface Incoming {
		record Show(int x, int y, int w, int h) implements Incoming {}

		record Hide() implements Incoming {}

		record Profile(Integer fps, Integer renderDistance, Integer previewIntervalMs, Integer previewWidth) implements Incoming {}

		record ChatSend(String text) implements Incoming {}

		record Reconnect() implements Incoming {}

		record Respawn() implements Incoming {}

		record Quit() implements Incoming {}

		record Hotkeys(String next, String prev, String nextAttention, String grid) implements Incoming {}
	}

	private Protocol() {}

	static String hello(String instance, String token, long pid, String modVersion) {
		JsonObject o = message("hello");
		o.addProperty("instance", instance);
		o.addProperty("token", token);
		o.addProperty("pid", pid);
		o.addProperty("mod_version", modVersion);
		return GSON.toJson(o);
	}

	static String state(String kind, String server, String reason, String cause) {
		JsonObject o = message("state");
		o.addProperty("kind", kind);
		o.addProperty("server", server);
		o.addProperty("reason", reason);
		o.addProperty("cause", cause);
		return GSON.toJson(o);
	}

	static String chat(String kind, String from, String text) {
		JsonObject o = message("chat");
		o.addProperty("kind", kind);
		o.addProperty("from", from);
		o.addProperty("text", text);
		return GSON.toJson(o);
	}

	static String preview(int seq, int w, int h) {
		JsonObject o = message("preview");
		o.addProperty("seq", Integer.toUnsignedLong(seq));
		o.addProperty("w", w);
		o.addProperty("h", h);
		return GSON.toJson(o);
	}

	static String hotkey(String action) {
		JsonObject o = message("hotkey");
		o.addProperty("action", action);
		return GSON.toJson(o);
	}

	static String window(boolean visible, boolean focused) {
		JsonObject o = message("window");
		o.addProperty("visible", visible);
		o.addProperty("focused", focused);
		return GSON.toJson(o);
	}

	static Optional<Incoming> parse(String line) {
		try {
			JsonElement root = JsonParser.parseString(line);
			if (!root.isJsonObject()) {
				return Optional.empty();
			}
			JsonObject o = root.getAsJsonObject();
			String type = string(o, "t");
			if (type == null) {
				return Optional.empty();
			}
			return Optional.ofNullable(switch (type) {
				case "show" -> new Incoming.Show(requireInt(o, "x"), requireInt(o, "y"), requireInt(o, "w"), requireInt(o, "h"));
				case "hide" -> new Incoming.Hide();
				case "profile" -> new Incoming.Profile(integer(o, "fps"), integer(o, "render_distance"), integer(o, "preview_interval_ms"), integer(o, "preview_width"));
				case "chat_send" -> string(o, "text") == null ? null : new Incoming.ChatSend(string(o, "text"));
				case "reconnect" -> new Incoming.Reconnect();
				case "respawn" -> new Incoming.Respawn();
				case "quit" -> new Incoming.Quit();
				case "hotkeys" -> new Incoming.Hotkeys(string(o, "next"), string(o, "prev"), string(o, "next_attention"), string(o, "grid"));
				default -> null;
			});
		} catch (RuntimeException e) {
			return Optional.empty();
		}
	}

	private static JsonObject message(String type) {
		JsonObject o = new JsonObject();
		o.addProperty("t", type);
		return o;
	}

	private static String string(JsonObject o, String key) {
		JsonElement e = o.get(key);
		return e != null && e.isJsonPrimitive() ? e.getAsString() : null;
	}

	private static Integer integer(JsonObject o, String key) {
		JsonElement e = o.get(key);
		if (e == null || !e.isJsonPrimitive()) {
			return null;
		}
		JsonPrimitive p = e.getAsJsonPrimitive();
		return p.isNumber() ? (int) Math.round(p.getAsDouble()) : null;
	}

	private static int requireInt(JsonObject o, String key) {
		Integer v = integer(o, key);
		if (v == null) {
			throw new IllegalArgumentException(key);
		}
		return v;
	}
}
