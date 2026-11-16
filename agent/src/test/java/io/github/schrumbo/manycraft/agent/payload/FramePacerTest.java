package io.github.schrumbo.manycraft.agent.payload;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

class FramePacerTest {
	private static final long SECOND = 1_000_000_000L;

	@Test
	void keepsTheCadenceWhenAFrameRunsLate() {
		Headless.FramePacer pacer = new Headless.FramePacer();
		long t = 5 * SECOND;
		assertEquals(SECOND / 4, pacer.next(t, 4));
		t += SECOND / 4 + SECOND / 20;
		assertEquals(SECOND / 5, pacer.next(t, 4));
	}

	@Test
	void restartsAfterAStallInsteadOfCatchingUp() {
		Headless.FramePacer pacer = new Headless.FramePacer();
		long t = 5 * SECOND;
		pacer.next(t, 3);
		t += 10 * SECOND;
		assertEquals(SECOND / 3, pacer.next(t, 3));
	}

	@Test
	void treatsAZeroRateAsOneFramePerSecond() {
		assertEquals(SECOND, new Headless.FramePacer().next(SECOND, 0));
	}
}
