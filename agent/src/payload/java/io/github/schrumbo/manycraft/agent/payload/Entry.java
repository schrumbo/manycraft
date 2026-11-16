package io.github.schrumbo.manycraft.agent.payload;

import io.github.schrumbo.manycraft.agent.Bridge;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.Map;

/**
 * What the agent calls once it loaded the payload next to the game.
 */
public final class Entry {
	private Entry() {}

	/**
	 * Starts the companion; null when the manager did not launch the game.
	 */
	public static Bridge install(Map<String, Boolean> applied) {
		return Companion.start(version());
	}

	private static String version() {
		try (InputStream in = ClassLoader.getSystemResourceAsStream("payload/version.txt")) {
			return in == null ? "agent" : "agent-" + new String(in.readAllBytes(), StandardCharsets.UTF_8).trim();
		} catch (IOException e) {
			return "agent";
		}
	}
}
