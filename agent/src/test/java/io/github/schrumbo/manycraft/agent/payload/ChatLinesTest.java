package io.github.schrumbo.manycraft.agent.payload;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;

import java.util.ArrayList;
import java.util.List;
import org.junit.jupiter.api.Test;

class ChatLinesTest {
	private static final class Fake implements ChatLines.Sink {
		boolean inGame = true;
		final List<String> sent = new ArrayList<>();
		final List<String> outcomes = new ArrayList<>();

		@Override
		public boolean inGame() {
			return inGame;
		}

		@Override
		public boolean send(String text) {
			sent.add(text);
			return true;
		}

		@Override
		public void outcome(Long id, String state, String why) {
			outcomes.add(id + ":" + state + (why == null ? "" : ":" + why));
		}
	}

	@Test
	void sendsABurstThenOnePerSecondInOrder() {
		Fake fake = new Fake();
		ChatLines lines = new ChatLines(fake);
		for (int i = 0; i < 10; i++) {
			lines.add((long) i, "/cmd " + i);
		}
		lines.tick();
		assertEquals(6, fake.sent.size());
		assertEquals("/cmd 0", fake.sent.get(0));
		assertEquals("/cmd 5", fake.sent.get(5));
		for (int t = 0; t < ChatLines.COST - 1; t++) {
			lines.tick();
		}
		assertEquals(6, fake.sent.size());
		lines.tick();
		assertEquals(7, fake.sent.size());
		assertEquals(3, lines.waiting());
	}

	@Test
	void neverGoesAboveTheServersLimit() {
		Fake fake = new Fake();
		ChatLines lines = new ChatLines(fake);
		for (int i = 0; i < 100; i++) {
			lines.add(null, "hi " + i);
		}
		int load = 0;
		int seen = 0;
		for (int t = 0; t < 4000; t++) {
			load = Math.max(0, load - 1);
			lines.tick();
			while (seen < fake.sent.size()) {
				seen++;
				load += 20;
				assertEquals(true, load <= 200, "load " + load + " at tick " + t);
			}
		}
		assertEquals(100, fake.sent.size());
	}

	@Test
	void refusesWithAReasonAndKeepsNothingBack() {
		Fake fake = new Fake();
		ChatLines lines = new ChatLines(fake);
		lines.add(1L, "x".repeat(257));
		lines.add(2L, "bad " + (char) 0xa7 + "c text");
		lines.add(3L, "   ");
		fake.inGame = false;
		lines.add(4L, "hello");
		assertEquals(List.of("1:refused:too_long", "2:refused:bad_text", "3:refused:bad_text", "4:refused:not_in_game"), fake.outcomes);
		assertEquals(0, fake.sent.size());
		assertNull(ChatLines.refusal("/home"));
	}

	@Test
	void dropsWhatWaitsWhenTheGameIsLeft() {
		Fake fake = new Fake();
		ChatLines lines = new ChatLines(fake);
		for (int i = 0; i < 8; i++) {
			lines.add((long) i, "line " + i);
		}
		lines.tick();
		fake.inGame = false;
		lines.tick();
		assertEquals(0, lines.waiting());
		assertEquals(List.of("6:refused:dropped", "7:refused:dropped"), fake.outcomes.subList(fake.outcomes.size() - 2, fake.outcomes.size()));
	}
}