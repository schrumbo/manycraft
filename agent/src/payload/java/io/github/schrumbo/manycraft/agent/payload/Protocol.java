package io.github.schrumbo.manycraft.agent.payload;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import com.google.gson.JsonPrimitive;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;

final class Protocol {
	private static final Gson GSON = new GsonBuilder().serializeNulls().disableHtmlEscaping().create();

	sealed interface Incoming {
		record Show(int x, int y, int w, int h, String mode) implements Incoming {}

		record Hide() implements Incoming {}

		record Profile(Integer fps, Integer renderDistance, Integer previewIntervalMs, Integer previewWidth) implements Incoming {}

		record ChatSend(String text) implements Incoming {}

		record ChatLine(long id, String text) implements Incoming {}

		record Reconnect() implements Incoming {}

		record Respawn() implements Incoming {}

		record Quit() implements Incoming {}

		record Hotkeys(String next, String prev, String nextAttention, String grid) implements Incoming {}

		record Identity(String name, String batch, int rgb, String icon) implements Incoming {}

		record Badge(boolean visible) implements Incoming {}

		record Hint(String text) implements Incoming {}

		record Level(BackgroundLevel level, BackgroundLevel.Custom custom) implements Incoming {}

		record Options(Map<String, String> values) implements Incoming {}

		record Servers() implements Incoming {}

		record Packs(String policy) implements Incoming {}

		record Answer(String id, boolean accept) implements Incoming {}

		record Capture(int width) implements Incoming {}
	}

	record Offer(String id, boolean required, String text) {}

	private Protocol() {}

	static String hello(String instance, String token, long pid, String modVersion, Map<String, Boolean> hooks) {
		JsonObject o = message("hello");
		o.addProperty("instance", instance);
		o.addProperty("token", token);
		o.addProperty("pid", pid);
		o.addProperty("mod_version", modVersion);
		JsonArray features = new JsonArray();
		features.add("chat_lines");
		hooks.forEach((name, applied) -> {
			if (applied) {
				features.add(name);
			}
		});
		o.add("features", features);
		return GSON.toJson(o);
	}

	static String state(String kind, String server, String reason, String cause) {
		return state(kind, server, reason, cause, null);
	}

	static String state(String kind, String server, String reason, String cause, Offer offer) {
		JsonObject o = message("state");
		o.addProperty("kind", kind);
		o.addProperty("server", server);
		o.addProperty("reason", reason);
		o.addProperty("cause", cause);
		if (offer != null) {
			JsonObject held = new JsonObject();
			held.addProperty("id", offer.id());
			held.addProperty("required", offer.required());
			held.addProperty("text", offer.text());
			o.add("offer", held);
		}
		return GSON.toJson(o);
	}

	record Run(String text, Integer color, boolean bold, boolean italic, boolean underline, boolean strike) {}

	static String chat(String kind, String from, String text) {
		return chat(kind, from, text, List.of());
	}

	static String chat(String kind, String from, String text, List<Run> runs) {
		JsonObject o = message("chat");
		o.addProperty("kind", kind);
		o.addProperty("from", from);
		o.addProperty("text", text);
		if (!runs.isEmpty()) {
			JsonArray list = new JsonArray();
			for (Run r : runs) {
				JsonObject j = new JsonObject();
				j.addProperty("t", r.text());
				if (r.color() != null) {
					j.addProperty("c", r.color());
				}
				if (r.bold()) {
					j.addProperty("b", true);
				}
				if (r.italic()) {
					j.addProperty("i", true);
				}
				if (r.underline()) {
					j.addProperty("u", true);
				}
				if (r.strike()) {
					j.addProperty("s", true);
				}
				list.add(j);
			}
			o.add("runs", list);
		}
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

	static String chatOutcome(long id, String state, String why) {
		JsonObject o = message("chat_outcome");
		o.addProperty("id", id);
		JsonObject outcome = new JsonObject();
		outcome.addProperty("state", state);
		if (why != null) {
			outcome.addProperty("why", why);
		}
		o.add("outcome", outcome);
		return GSON.toJson(o);
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
				case "chat_line" -> string(o, "text") == null || o.get("id") == null ? null : new Incoming.ChatLine(o.get("id").getAsLong(), string(o, "text"));
				case "reconnect" -> new Incoming.Reconnect();
				case "respawn" -> new Incoming.Respawn();
				case "quit" -> new Incoming.Quit();
				case "hotkeys" -> new Incoming.Hotkeys(string(o, "next"), string(o, "prev"), string(o, "next_attention"), string(o, "grid"));
				case "identity" -> string(o, "name") == null || string(o, "batch") == null ? null : new Incoming.Identity(string(o, "name"), string(o, "batch"), requireInt(o, "rgb") & 0xFFFFFF, string(o, "icon"));
				case "badge" -> bool(o, "visible") == null ? null : new Incoming.Badge(bool(o, "visible"));
				case "hint" -> string(o, "text") == null ? null : new Incoming.Hint(string(o, "text"));
				case "level" -> BackgroundLevel.parse(string(o, "level")).map(l -> new Incoming.Level(l, BackgroundLevel.Custom.of(integer(o, "fps"), integer(o, "render_distance")))).orElse(null);
				case "options" -> strings(o, "values") == null ? null : new Incoming.Options(strings(o, "values"));
				case "servers" -> new Incoming.Servers();
				case "packs" -> string(o, "policy") == null ? null : new Incoming.Packs(string(o, "policy"));
				case "answer" -> string(o, "id") == null || bool(o, "accept") == null ? null : new Incoming.Answer(string(o, "id"), bool(o, "accept"));
				case "capture" -> new Incoming.Capture(requireInt(o, "width"));
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