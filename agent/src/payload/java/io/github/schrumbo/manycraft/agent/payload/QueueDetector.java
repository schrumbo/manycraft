package io.github.schrumbo.manycraft.agent.payload;

import java.util.regex.Pattern;

final class QueueDetector {
	private static final Pattern QUEUE = Pattern.compile("(?i)(position in queue|queue position|in queue|queued)");
	private static final long WINDOW_MS = 30_000L;

	private long lastSeen = Long.MIN_VALUE;

	void onSystemMessage(String text, long nowMs) {
		if (QUEUE.matcher(text).find()) {
			lastSeen = nowMs;
		}
	}

	boolean active(long nowMs) {
		return lastSeen != Long.MIN_VALUE && nowMs - lastSeen <= WINDOW_MS;
	}

	void reset() {
		lastSeen = Long.MIN_VALUE;
	}
}
