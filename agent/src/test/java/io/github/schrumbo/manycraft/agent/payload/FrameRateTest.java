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
		assertEquals(4.0, rate.count(t).orElseThrow().fps(), 1e-9);
		for (int i = 0; i < 9; i++) {
			t += SECOND / 2;
			assertTrue(rate.count(t).isEmpty());
		}
		t += SECOND / 2;
		assertEquals(2.0, rate.count(t).orElseThrow().fps(), 1e-9);
	}

	@Test
	void reportsTheWorstFrameAndSlowFramesOfEachWindow() {
		FrameRate rate = new FrameRate(SECOND);
		long t = SECOND;
		rate.count(t);
		for (int i = 0; i < 9; i++) {
			t += i == 4 ? 120_000_000L : 10_000_000L;
			rate.count(t);
		}
		t += SECOND;
		FrameRate.Window first = rate.count(t).orElseThrow();
		assertEquals(1000, first.worstMs());
		assertEquals(2, first.slow());
		t += 10_000_000L;
		rate.count(t);
		t += SECOND;
		FrameRate.Window second = rate.count(t).orElseThrow();
		assertEquals(1000, second.worstMs());
		assertEquals(1, second.slow());
	}
}
