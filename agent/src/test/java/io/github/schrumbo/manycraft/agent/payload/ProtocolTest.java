package io.github.schrumbo.manycraft.agent.payload;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import java.util.Map;
import java.util.TreeMap;
import org.junit.jupiter.api.Test;

class ProtocolTest {
	private static JsonObject json(String line) {
		assertFalse(line.contains("\n"));
		return JsonParser.parseString(line).getAsJsonObject();
	}

	@Test
	void helloCarriesIdentity() {
		JsonObject o = json(Protocol.hello("a1", "sek", 42L, "0.1.0", new TreeMap<>(Map.of("core", true, "proxy", false, "options", true))));
		assertEquals("hello", o.get("t").getAsString());
		assertEquals("a1", o.get("instance").getAsString());
		assertEquals("sek", o.get("token").getAsString());
		assertEquals(42L, o.get("pid").getAsLong());
		assertEquals("0.1.0", o.get("mod_version").getAsString());
		assertEquals("[\"chat_lines\",\"core\",\"options\"]", o.get("features").toString());
	}

	@Test
	void stateKeepsExplicitNulls() {
		JsonObject o = json(Protocol.state("menu", null, null, null));
		assertTrue(o.has("server") && o.get("server").isJsonNull());
		assertTrue(o.has("reason") && o.get("reason").isJsonNull());
		o = json(Protocol.state("disconnected", "mc.example:25565", "Test", "banned"));
		assertEquals("disconnected", o.get("kind").getAsString());
		assertEquals("mc.example:25565", o.get("server").getAsString());
		assertEquals("Test", o.get("reason").getAsString());
		assertEquals("banned", o.get("cause").getAsString());
	}

	@Test
	void chatEncodesKindFromAndText() {
		JsonObject o = json(Protocol.chat("player", "Rcon", "hi \"there\" <3"));
		assertEquals("player", o.get("kind").getAsString());
		assertEquals("Rcon", o.get("from").getAsString());
		assertEquals("hi \"there\" <3", o.get("text").getAsString());
		assertTrue(json(Protocol.chat("system", null, "x")).get("from").isJsonNull());
		assertFalse(o.has("runs"));
	}

	@Test
	void chatCarriesStyledRuns() {
		var runs = java.util.List.of(new Protocol.Run("<Rcon> ", 0xFFAA00, true, false, false, false), new Protocol.Run("hi", null, false, true, true, true));
		JsonObject o = json(Protocol.chat("player", "Rcon", "hi", runs));
		var list = o.getAsJsonArray("runs");
		assertEquals(2, list.size());
		JsonObject a = list.get(0).getAsJsonObject();
		assertEquals("<Rcon> ", a.get("t").getAsString());
		assertEquals(0xFFAA00, a.get("c").getAsInt());
		assertTrue(a.get("b").getAsBoolean());
		assertFalse(a.has("i"));
		JsonObject b = list.get(1).getAsJsonObject();
		assertFalse(b.has("c"));
		assertTrue(b.get("i").getAsBoolean() && b.get("u").getAsBoolean() && b.get("s").getAsBoolean());
	}

	@Test
	void stateCarriesAHeldOfferOnlyWhenThereIsOne() {
		assertFalse(json(Protocol.state("playing", "mc.example:25565", null, null)).has("offer"));
		JsonObject o = json(Protocol.state("offer", "mc.example:25565", null, null, new Protocol.Offer("6f1c", true, "Please")));
		assertEquals("offer", o.get("kind").getAsString());
		JsonObject offer = o.getAsJsonObject("offer");
		assertEquals("6f1c", offer.get("id").getAsString());
		assertTrue(offer.get("required").getAsBoolean());
		assertEquals("Please", offer.get("text").getAsString());
		assertTrue(json(Protocol.state("offer", null, null, null, new Protocol.Offer("a", false, null))).getAsJsonObject("offer").get("text").isJsonNull());
	}

	@Test
	void parsesPacksAnswerAndCapture() {
		assertEquals(new Protocol.Incoming.Packs("decline"), Protocol.parse("{\"t\":\"packs\",\"policy\":\"decline\"}").orElseThrow());
		assertEquals(new Protocol.Incoming.Answer("6f1c", true), Protocol.parse("{\"t\":\"answer\",\"id\":\"6f1c\",\"accept\":true}").orElseThrow());
		assertEquals(new Protocol.Incoming.Capture(640), Protocol.parse("{\"t\":\"capture\",\"width\":640}").orElseThrow());
		assertTrue(Protocol.parse("{\"t\":\"answer\",\"id\":\"6f1c\"}").isEmpty());
		assertTrue(Protocol.parse("{\"t\":\"capture\"}").isEmpty());
		assertTrue(Protocol.parse("{\"t\":\"packs\"}").isEmpty());
	}

	@Test
	void previewSeqIsUnsigned() {
		JsonObject o = json(Protocol.preview(0xFFFFFFFE, 320, 180));
		assertEquals(4294967294L, o.get("seq").getAsLong());
		assertEquals(320, o.get("w").getAsInt());
		assertEquals(180, o.get("h").getAsInt());
	}

	@Test
	void hotkeyAndWindow() {
		assertEquals("next_attention", json(Protocol.hotkey("next_attention")).get("action").getAsString());
		JsonObject w = json(Protocol.window(true, false));
		assertTrue(w.get("visible").getAsBoolean());
		assertFalse(w.get("focused").getAsBoolean());
	}

	@Test
	void parsesShowAndHide() {
		var show = assertInstanceOf(Protocol.Incoming.Show.class, Protocol.parse("{\"t\":\"show\",\"x\":10,\"y\":-20,\"w\":800,\"h\":600}").orElseThrow());
		assertEquals(new Protocol.Incoming.Show(10, -20, 800, 600, null), show);
		var full = Protocol.parse("{\"t\":\"show\",\"x\":0,\"y\":0,\"w\":800,\"h\":600,\"mode\":\"fullscreen\"}").orElseThrow();
		assertEquals(new Protocol.Incoming.Show(0, 0, 800, 600, "fullscreen"), full);
		assertInstanceOf(Protocol.Incoming.Hide.class, Protocol.parse("{\"t\":\"hide\"}").orElseThrow());
	}

	@Test
	void parsesProfileWithMissingFields() {
		var profile = assertInstanceOf(Protocol.Incoming.Profile.class, Protocol.parse("{\"t\":\"profile\",\"fps\":2,\"preview_width\":640}").orElseThrow());
		assertEquals(2, profile.fps());
		assertNull(profile.renderDistance());
		assertNull(profile.previewIntervalMs());
		assertEquals(640, profile.previewWidth());
	}

	@Test
	void parsesChatSendReconnectRespawnQuitHotkeys() {
		assertEquals("/say hi", assertInstanceOf(Protocol.Incoming.ChatSend.class, Protocol.parse("{\"t\":\"chat_send\",\"text\":\"/say hi\"}").orElseThrow()).text());
		assertEquals("× quits", assertInstanceOf(Protocol.Incoming.Hint.class, Protocol.parse("{\"t\":\"hint\",\"text\":\"× quits\"}").orElseThrow()).text());
		assertTrue(Protocol.parse("{\"t\":\"hint\"}").isEmpty());
		assertInstanceOf(Protocol.Incoming.Reconnect.class, Protocol.parse("{\"t\":\"reconnect\"}").orElseThrow());
		assertInstanceOf(Protocol.Incoming.Respawn.class, Protocol.parse("{\"t\":\"respawn\"}").orElseThrow());
		assertInstanceOf(Protocol.Incoming.Quit.class, Protocol.parse("{\"t\":\"quit\"}").orElseThrow());
		var keys = assertInstanceOf(Protocol.Incoming.Hotkeys.class, Protocol.parse("{\"t\":\"hotkeys\",\"next\":\"ctrl+tab\",\"grid\":\"f9\"}").orElseThrow());
		assertEquals("ctrl+tab", keys.next());
		assertNull(keys.prev());
		assertEquals("f9", keys.grid());
	}

	@Test
	void parsesIdentityBadgeAndLevel() {
		var id = assertInstanceOf(Protocol.Incoming.Identity.class, Protocol.parse("{\"t\":\"identity\",\"name\":\"Alt\",\"batch\":\"Farm\",\"rgb\":1193046,\"icon\":\"icon.png\"}").orElseThrow());
		assertEquals(new Protocol.Incoming.Identity("Alt", "Farm", 0x123456, "icon.png"), id);
		var bare = assertInstanceOf(Protocol.Incoming.Identity.class, Protocol.parse("{\"t\":\"identity\",\"name\":\"Alt\",\"batch\":\"Farm\",\"rgb\":0,\"icon\":null}").orElseThrow());
		assertNull(bare.icon());
		assertEquals("Alt · Farm", GameIdentity.title(id.name(), id.batch()));
		assertFalse(assertInstanceOf(Protocol.Incoming.Badge.class, Protocol.parse("{\"t\":\"badge\",\"visible\":false}").orElseThrow()).visible());
		assertEquals(BackgroundLevel.SAVING, assertInstanceOf(Protocol.Incoming.Level.class, Protocol.parse("{\"t\":\"level\",\"level\":\"saving\"}").orElseThrow()).level());
		assertEquals(BackgroundLevel.BALANCED, assertInstanceOf(Protocol.Incoming.Level.class, Protocol.parse("{\"t\":\"level\",\"level\":\"balanced\"}").orElseThrow()).level());
		assertEquals(BackgroundLevel.FULL, assertInstanceOf(Protocol.Incoming.Level.class, Protocol.parse("{\"t\":\"level\",\"level\":\"full\"}").orElseThrow()).level());
		var custom = assertInstanceOf(Protocol.Incoming.Level.class, Protocol.parse("{\"t\":\"level\",\"level\":\"custom\",\"fps\":10,\"render_distance\":6}").orElseThrow());
		assertEquals(new Protocol.Incoming.Level(BackgroundLevel.CUSTOM, new BackgroundLevel.Custom(10, 6)), custom);
		assertEquals(BackgroundLevel.Custom.DEFAULT, assertInstanceOf(Protocol.Incoming.Level.class, Protocol.parse("{\"t\":\"level\",\"level\":\"custom\"}").orElseThrow()).custom());
		assertTrue(Protocol.parse("{\"t\":\"level\",\"level\":\"turbo\"}").isEmpty());
		assertTrue(Protocol.parse("{\"t\":\"badge\",\"visible\":\"yes\"}").isEmpty());
		assertTrue(Protocol.parse("{\"t\":\"identity\",\"name\":\"Alt\",\"rgb\":1}").isEmpty());
		assertTrue(Protocol.parse("{\"t\":\"identity\",\"name\":\"Alt\",\"batch\":\"Farm\"}").isEmpty());
	}

	@Test
	void rejectsGarbage() {
		assertTrue(Protocol.parse("not json").isEmpty());
		assertTrue(Protocol.parse("[1,2]").isEmpty());
		assertTrue(Protocol.parse("{}").isEmpty());
		assertTrue(Protocol.parse("{\"t\":\"nope\"}").isEmpty());
		assertTrue(Protocol.parse("{\"t\":\"show\",\"x\":1}").isEmpty());
		assertTrue(Protocol.parse("{\"t\":\"chat_send\"}").isEmpty());
		assertEquals(new Protocol.Incoming.ChatLine(7, "/home"), Protocol.parse("{\"t\":\"chat_line\",\"id\":7,\"text\":\"/home\"}").orElseThrow());
		assertTrue(Protocol.parse("{\"t\":\"chat_line\",\"text\":\"x\"}").isEmpty());
		assertEquals("{\"t\":\"chat_outcome\",\"id\":7,\"outcome\":{\"state\":\"refused\",\"why\":\"dropped\"}}", Protocol.chatOutcome(7, "refused", "dropped"));
		assertEquals("{\"t\":\"chat_outcome\",\"id\":8,\"outcome\":{\"state\":\"sent\"}}", Protocol.chatOutcome(8, "sent", null));
	}

	@Test
	void sharedOptionsAndServersTravelBothWays() {
		var values = new java.util.LinkedHashMap<String, String>();
		values.put("guiScale", "3");
		values.put("resourcePacks", "[\"vanilla\",\"file/x.zip\"]");
		JsonObject o = json(Protocol.options(values));
		assertEquals("options", o.get("t").getAsString());
		assertEquals("[\"vanilla\",\"file/x.zip\"]", o.getAsJsonObject("values").get("resourcePacks").getAsString());
		var parsed = assertInstanceOf(Protocol.Incoming.Options.class, Protocol.parse(Protocol.options(values)).orElseThrow());
		assertEquals(values, parsed.values());
		assertEquals("servers", json(Protocol.servers()).get("t").getAsString());
		assertInstanceOf(Protocol.Incoming.Servers.class, Protocol.parse("{\"t\":\"servers\"}").orElseThrow());
		assertTrue(Protocol.parse("{\"t\":\"options\"}").isEmpty());
		assertTrue(Protocol.parse("{\"t\":\"options\",\"values\":[1]}").isEmpty());
		var skipped = assertInstanceOf(Protocol.Incoming.Options.class, Protocol.parse("{\"t\":\"options\",\"values\":{\"a\":\"1\",\"b\":{}}}").orElseThrow());
		assertEquals(java.util.Map.of("a", "1"), skipped.values());
	}
}