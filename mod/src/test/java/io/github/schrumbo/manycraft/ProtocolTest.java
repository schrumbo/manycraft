package io.github.schrumbo.manycraft;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import org.junit.jupiter.api.Test;

class ProtocolTest {
	private static JsonObject json(String line) {
		assertFalse(line.contains("\n"));
		return JsonParser.parseString(line).getAsJsonObject();
	}

	@Test
	void helloCarriesIdentity() {
		JsonObject o = json(Protocol.hello("a1", "sek", 42L, "0.1.0"));
		assertEquals("hello", o.get("t").getAsString());
		assertEquals("a1", o.get("instance").getAsString());
		assertEquals("sek", o.get("token").getAsString());
		assertEquals(42L, o.get("pid").getAsLong());
		assertEquals("0.1.0", o.get("mod_version").getAsString());
	}

	@Test
	void stateKeepsExplicitNulls() {
		JsonObject o = json(Protocol.state("menu", null, null));
		assertTrue(o.has("server") && o.get("server").isJsonNull());
		assertTrue(o.has("reason") && o.get("reason").isJsonNull());
		o = json(Protocol.state("disconnected", "mc.example:25565", "Test"));
		assertEquals("disconnected", o.get("kind").getAsString());
		assertEquals("mc.example:25565", o.get("server").getAsString());
		assertEquals("Test", o.get("reason").getAsString());
	}

	@Test
	void chatEncodesKindFromAndText() {
		JsonObject o = json(Protocol.chat("whisper", "Rcon", "hi \"there\" <3"));
		assertEquals("whisper", o.get("kind").getAsString());
		assertEquals("Rcon", o.get("from").getAsString());
		assertEquals("hi \"there\" <3", o.get("text").getAsString());
		assertTrue(json(Protocol.chat("system", null, "x")).get("from").isJsonNull());
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
		assertEquals(new Protocol.Incoming.Show(10, -20, 800, 600), show);
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
		assertInstanceOf(Protocol.Incoming.Reconnect.class, Protocol.parse("{\"t\":\"reconnect\"}").orElseThrow());
		assertInstanceOf(Protocol.Incoming.Respawn.class, Protocol.parse("{\"t\":\"respawn\"}").orElseThrow());
		assertInstanceOf(Protocol.Incoming.Quit.class, Protocol.parse("{\"t\":\"quit\"}").orElseThrow());
		var keys = assertInstanceOf(Protocol.Incoming.Hotkeys.class, Protocol.parse("{\"t\":\"hotkeys\",\"next\":\"ctrl+tab\",\"grid\":\"f9\"}").orElseThrow());
		assertEquals("ctrl+tab", keys.next());
		assertNull(keys.prev());
		assertEquals("f9", keys.grid());
	}

	@Test
	void rejectsGarbage() {
		assertTrue(Protocol.parse("not json").isEmpty());
		assertTrue(Protocol.parse("[1,2]").isEmpty());
		assertTrue(Protocol.parse("{}").isEmpty());
		assertTrue(Protocol.parse("{\"t\":\"nope\"}").isEmpty());
		assertTrue(Protocol.parse("{\"t\":\"show\",\"x\":1}").isEmpty());
		assertTrue(Protocol.parse("{\"t\":\"chat_send\"}").isEmpty());
	}
}
