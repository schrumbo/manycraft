package io.github.schrumbo.manycraft.agent.payload;

import java.util.ArrayDeque;

/**
 * Lines the user typed, waiting for their turn under the server's spam limit: the server adds 20 to a counter per chat
 * line or command, takes 1 per tick off it and kicks above 200. A burst of six, then one per second.
 */
final class ChatLines {
	static final int COST = 20;
	static final int BURST = 6;
	static final int MAX_LENGTH = 256;
	private static final int LIMIT = (BURST - 1) * COST;

	interface Sink {
		boolean inGame();

		boolean send(String text);

		void outcome(Long id, String state, String why);
	}

	private record Line(Long id, String text) {}

	private final Sink sink;
	private final ArrayDeque<Line> lines = new ArrayDeque<>();
	private int load;

	ChatLines(Sink sink) {
		this.sink = sink;
	}

	/** Why the trimmed text cannot go out as a line, or null. */
	static String refusal(String text) {
		if (text.codePointCount(0, text.length()) > MAX_LENGTH) {
			return "too_long";
		}
		if (text.isEmpty() || text.chars().anyMatch(c -> c == 0xa7 || c < ' ' || c == 0x7f)) {
			return "bad_text";
		}
		return null;
	}

	void add(Long id, String text) {
		String trimmed = text.trim();
		String why = sink.inGame() ? refusal(trimmed) : "not_in_game";
		if (why != null) {
			sink.outcome(id, "refused", why);
			return;
		}
		lines.add(new Line(id, trimmed));
	}

	void tick() {
		load = Math.max(0, load - 1);
		if (lines.isEmpty()) {
			return;
		}
		if (!sink.inGame()) {
			drop();
			return;
		}
		while (!lines.isEmpty() && load <= LIMIT) {
			Line line = lines.poll();
			load += COST;
			if (sink.send(line.text())) {
				sink.outcome(line.id(), "sent", null);
			} else {
				sink.outcome(line.id(), "refused", "failed");
			}
		}
	}

	int waiting() {
		return lines.size();
	}

	private void drop() {
		Line line;
		while ((line = lines.poll()) != null) {
			sink.outcome(line.id(), "refused", "dropped");
		}
	}
}
