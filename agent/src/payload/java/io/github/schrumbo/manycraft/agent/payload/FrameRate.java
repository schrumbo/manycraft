package io.github.schrumbo.manycraft.agent.payload;

import java.util.OptionalDouble;

final class FrameRate {
	private final long window;
	private long start;
	private int frames;

	FrameRate(long windowNanos) {
		this.window = windowNanos;
	}

	OptionalDouble count(long now) {
		if (frames == 0 && start == 0) {
			start = now;
		}
		frames++;
		long elapsed = now - start;
		if (elapsed < window) {
			return OptionalDouble.empty();
		}
		double fps = (frames - 1) * 1e9 / elapsed;
		start = now;
		frames = 1;
		return OptionalDouble.of(fps);
	}
}
