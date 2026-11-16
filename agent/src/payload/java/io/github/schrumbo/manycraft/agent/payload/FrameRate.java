package io.github.schrumbo.manycraft.agent.payload;

import java.util.Optional;

final class FrameRate {
	private static final long SLOW = 50_000_000L;

	private final long window;
	private long start;
	private long last;
	private long worst;
	private int frames;
	private int slow;

	record Window(double fps, long worstMs, int slow) {
	}

	FrameRate(long windowNanos) {
		this.window = windowNanos;
	}

	Optional<Window> count(long now) {
		if (frames == 0 && start == 0) {
			start = now;
			last = now;
		}
		long gap = now - last;
		last = now;
		worst = Math.max(worst, gap);
		if (gap > SLOW) {
			slow++;
		}
		frames++;
		long elapsed = now - start;
		if (elapsed < window) {
			return Optional.empty();
		}
		Window done = new Window((frames - 1) * 1e9 / elapsed, worst / 1_000_000, slow);
		start = now;
		frames = 1;
		worst = 0;
		slow = 0;
		return Optional.of(done);
	}
}
