package io.github.schrumbo.manycraft;

import static org.junit.jupiter.api.Assertions.assertEquals;
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
		assertFalse(s.due(2_999_000_000L, 3000));
		assertTrue(s.due(3_000_000_000L, 3000));
	}

	@Test
	void previewScheduleTakesTheFrameNearestTheInterval() {
		PreviewSchedule s = new PreviewSchedule();
		s.mark(0);
		assertFalse(s.due(833_000_000L, 1000, 333));
		assertTrue(s.due(999_000_000L, 1000, 333));
		assertEquals(8, BackgroundController.previewFramerate(250));
		assertEquals(2, BackgroundController.previewFramerate(1000));
		assertEquals(BackgroundController.PREVIEW_FPS_MAX, BackgroundController.previewFramerate(50));
	}

	@Test
	void previewScheduleSoonFiresOnTheNextFrame() {
		PreviewSchedule s = new PreviewSchedule();
		s.mark(0);
		assertFalse(s.due(1_000_000L, 30_000));
		s.soon();
		assertTrue(s.due(1_000_000L, 30_000));
		s.mark(1_000_000L);
		assertFalse(s.due(2_000_000L, 30_000));
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
