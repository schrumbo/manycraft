package io.github.schrumbo.manycraft.agent.payload;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;

class QueueDetectorTest {
	@Test
	void activeForWindowAfterQueueMessage() {
		QueueDetector q = new QueueDetector();
		assertFalse(q.active(0));
		q.onSystemMessage("Welcome", 1000);
		assertFalse(q.active(1000));
		q.onSystemMessage("Position in queue: 12", 1000);
		assertTrue(q.active(20_000));
		assertFalse(q.active(40_000));
		q.onSystemMessage("Position in queue: 3", 40_000);
		q.reset();
		assertFalse(q.active(40_001));
	}

	@Test
	void previewScheduleFiresFirstThenOnInterval() {
		PreviewSchedule s = new PreviewSchedule();
		assertTrue(s.due(0, 3000));
		s.mark(0);
		assertFalse(s.due(2_600_000_000L, 3000));
		assertTrue(s.due(2_625_000_000L, 3000));
		s.mark(0);
		assertFalse(s.due(124_000_000L, 250));
		assertTrue(s.due(249_000_000L, 250));
	}

	@Test
	void stripsChatDecoration() {
		String template = "Rcon whispers to you: \u0000";
		org.junit.jupiter.api.Assertions.assertEquals("hi", ChatBridge.stripDecoration("Rcon whispers to you: hi", template));
		org.junit.jupiter.api.Assertions.assertEquals("[Rcon] x", ChatBridge.stripDecoration("[Rcon] x", template));
		org.junit.jupiter.api.Assertions.assertEquals("a", ChatBridge.stripDecoration("<Dev> a", "<Dev> \u0000"));
		org.junit.jupiter.api.Assertions.assertEquals("t", ChatBridge.stripDecoration("(t)", "(\u0000)"));
	}
}
