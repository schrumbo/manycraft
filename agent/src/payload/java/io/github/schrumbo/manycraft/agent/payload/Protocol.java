package io.github.schrumbo.manycraft.agent.payload;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import com.google.gson.JsonPrimitive;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Optional;

final class Protocol {
	private static final Gson GSON = new GsonBuilder().serializeNulls().disableHtmlEscaping().create();

	sealed interface Incoming {
		record Show(int x, int y, int w, int h, String mode) implements Incoming {}

		record Hide() implements Incoming {}

		record Profile(Integer fps, Integer renderDistance, Integer previewIntervalMs, Integer previewWidth) implements Incoming {}

		record ChatSend(String text) implements Incoming {}

		record Reconnect() implements Incoming {}

		record Respawn() implements Incoming {}

		record Quit() implements Incoming {}

		record Hotkeys(String next, String prev, String nextAttention, String grid) implements Incoming {}

		record Identity(String name, String batch, int rgb, String icon) implements Incoming {}

		record Badge(boolean visible) implements Incoming {}

		record Level(BackgroundLevel level, BackgroundLevel.Custom custom) implements Incoming {}

		record Options(Map<String, String> values) implements Incoming {}

		record Servers() implements Incoming {}
	}

	private Protocol() {}

	static String hello(String instance, String token, long pid, String modVersion, Map<String, Boolean> hooks) {
		JsonObject o = message("hello");
		o.addProperty("instance", instance);
		o.addProperty("token", token);
		o.addProperty("pid", pid);
		o.addProperty("mod_version", modVersion);
		JsonArray features = new JsonArray();
		hooks.forEach((name, applied) -> {
			if (applied) {
				features.add(name);
			}
		});
		o.add("features", features);
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

	static String closing() {
		return GSON.toJson(message("closing"));
	}

	static String window(boolean visible, boolean focused) {
		JsonObject o = message("window");
		o.addProperty("visible", visible);
		o.addProperty("focused", focused);
		return GSON.toJson(o);
	}

	static String options(Map<String, String> values) {
		JsonObject o = message("options");
		JsonObject v = new JsonObject();
		values.forEach(v::addProperty);
		o.add("values", v);
		return GSON.toJson(o);
	}

	static String servers() {
		return GSON.toJson(message("servers"));
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
				case "show" -> new Incoming.Show(requireInt(o, "x"), requireInt(o, "y"), requireInt(o, "w"), requireInt(o, "h"), string(o, "mode"));
				case "hide" -> new Incoming.Hide();
				case "profile" -> new Incoming.Profile(integer(o, "fps"), integer(o, "render_distance"), integer(o, "preview_interval_ms"), integer(o, "preview_width"));
				case "chat_send" -> string(o, "text") == null ? null : new Incoming.ChatSend(string(o, "text"));
				case "reconnect" -> new Incoming.Reconnect();
				case "respawn" -> new Incoming.Respawn();
				case "quit" -> new Incoming.Quit();
				case "hotkeys" -> new Incoming.Hotkeys(string(o, "next"), string(o, "prev"), string(o, "next_attention"), string(o, "grid"));
				case "identity" -> string(o, "name") == null || string(o, "batch") == null ? null : new Incoming.Identity(string(o, "name"), string(o, "batch"), requireInt(o, "rgb") & 0xFFFFFF, string(o, "icon"));
				case "badge" -> bool(o, "visible") == null ? null : new Incoming.Badge(bool(o, "visible"));
				case "level" -> BackgroundLevel.parse(string(o, "level")).map(l -> new Incoming.Level(l, BackgroundLevel.Custom.of(integer(o, "fps"), integer(o, "render_distance")))).orElse(null);
				case "options" -> strings(o, "values") == null ? null : new Incoming.Options(strings(o, "values"));
				case "servers" -> new Incoming.Servers();
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

	private static Map<String, String> strings(JsonObject o, String key) {
		JsonElement e = o.get(key);
		if (e == null || !e.isJsonObject()) {
			return null;
		}
		Map<String, String> values = new LinkedHashMap<>();
		for (Map.Entry<String, JsonElement> entry : e.getAsJsonObject().entrySet()) {
			if (entry.getValue().isJsonPrimitive()) {
				values.put(entry.getKey(), entry.getValue().getAsString());
			}
		}
		return values;
	}

	private static Boolean bool(JsonObject o, String key) {
		JsonElement e = o.get(key);
		return e != null && e.isJsonPrimitive() && e.getAsJsonPrimitive().isBoolean() ? e.getAsBoolean() : null;
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
