package io.github.schrumbo.manycraft.agent.payload;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;

class FrameRateTest {
	private static final long SECOND = 1_000_000_000L;

	@Test
	void reportsFramesPerSecondOncePerWindow() {
		FrameRate rate = new FrameRate(5 * SECOND);
		long t = 7 * SECOND;
		for (int i = 0; i < 20; i++) {
			assertTrue(rate.count(t).isEmpty());
			t += SECOND / 4;
		}
		assertEquals(4.0, rate.count(t).orElseThrow(), 1e-9);
		for (int i = 0; i < 9; i++) {
			t += SECOND / 2;
			assertTrue(rate.count(t).isEmpty());
		}
		t += SECOND / 2;
		assertEquals(2.0, rate.count(t).orElseThrow(), 1e-9);
	}
}
