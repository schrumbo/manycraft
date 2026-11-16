package io.github.schrumbo.manycraft;

final class PreviewSchedule {
	private long lastNanos;
	private boolean taken;

	boolean due(long nowNanos, long intervalMs) {
		return due(nowNanos, intervalMs, 0);
	}

	/** Whether a capture is due on a frame that comes every {@code frameMs}: half a frame early beats a whole frame late. */
	boolean due(long nowNanos, long intervalMs, long frameMs) {
		return !taken || nowNanos - lastNanos >= (intervalMs * 2 - frameMs) * 500_000L;
	}

	void soon() {
		taken = false;
	}

	void mark(long nowNanos) {
		taken = true;
		lastNanos = nowNanos;
	}
}
