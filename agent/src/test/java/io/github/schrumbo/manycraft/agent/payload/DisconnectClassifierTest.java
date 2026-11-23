package io.github.schrumbo.manycraft.agent.payload;

import static org.junit.jupiter.api.Assertions.assertEquals;

import java.util.List;
import org.junit.jupiter.api.Test;

class DisconnectClassifierTest {
	private static String keyed(String key) {
		return DisconnectClassifier.classify(List.of(key), "");
	}

	private static String texted(String text) {
		return DisconnectClassifier.classify(List.of(), text);
	}

	@Test
	void vanillaKeysMapToBuckets() {
		assertEquals("banned", keyed("multiplayer.disconnect.banned.reason"));
		assertEquals("banned", keyed("multiplayer.disconnect.banned_ip.expiration"));
		assertEquals("whitelist", keyed("multiplayer.disconnect.not_whitelisted"));
		assertEquals("server_full", keyed("multiplayer.disconnect.server_full"));
		assertEquals("pack_declined", keyed("multiplayer.requiredTexturePrompt.disconnect"));
		assertEquals("kicked", keyed("multiplayer.disconnect.kicked"));
		assertEquals("kicked", keyed("multiplayer.disconnect.idling"));
		assertEquals("duplicate", keyed("multiplayer.disconnect.duplicate_login"));
		assertEquals("server_down", keyed("multiplayer.disconnect.server_shutdown"));
		assertEquals("server_down", keyed("disconnect.timeout"));
		assertEquals("server_down", keyed("disconnect.lost"));
		assertEquals("server_down", keyed("multiplayer.disconnect.authservers_down"));
		assertEquals("auth", keyed("multiplayer.disconnect.unverified_username"));
		assertEquals("auth", keyed("disconnect.loginFailedInfo.invalidSession"));
		assertEquals("server_down", keyed("disconnect.loginFailedInfo.serversUnavailable"));
		assertEquals("unknown", keyed("multiplayer.disconnect.generic"));
	}

	@Test
	void wrappedLoginFailuresUseTheNestedKey() {
		String wrapper = "disconnect.loginFailedInfo";
		assertEquals("server_down", DisconnectClassifier.classify(List.of(wrapper, "disconnect.loginFailedInfo.serversUnavailable"), ""));
		assertEquals("banned", DisconnectClassifier.classify(List.of(wrapper, "disconnect.loginFailedInfo.userBanned"), ""));
		assertEquals("auth", DisconnectClassifier.classify(List.of(wrapper, "disconnect.loginFailedInfo.invalidSession"), ""));
		assertEquals("auth", DisconnectClassifier.classify(List.of(wrapper), ""));
	}

	@Test
	void keysWinOverText() {
		assertEquals("kicked", DisconnectClassifier.classify(List.of("multiplayer.disconnect.kicked"), "Banned for flying"));
	}

	@Test
	void customTextIsMatched() {
		assertEquals("banned", texted("You are permanently banned from this server!"));
		assertEquals("banned", texted("Your account is blacklisted"));
		assertEquals("banned", texted("Du wurdest vom Netzwerk gebannt"));
		assertEquals("banned", texted("Du bist gesperrt"));
		assertEquals("banned", texted("You have been tempbanned for 3 days"));
		assertEquals("duplicate", texted("You logged in from another location"));
		assertEquals("whitelist", texted("You are not white-listed on this server!"));
		assertEquals("server_full", texted("The server is full"));
		assertEquals("kicked", texted("Kicked for being idle"));
		assertEquals("server_down", texted("Connection refused: getsockopt"));
		assertEquals("server_down", texted("Server is restarting"));
		assertEquals("auth", texted("Failed to verify username!"));
		assertEquals("unknown", texted("Something odd happened"));
		assertEquals("unknown", texted(""));
	}

	@Test
	void banNeedsToBeAWord() {
		assertEquals("unknown", texted("Please abandon the urban area"));
	}
}