package io.github.schrumbo.manycraft;

final class PreviewSchedule {
	private long lastNanos;
	private boolean taken;

	boolean due(long nowNanos, long intervalMs) {
		return !taken || nowNanos - lastNanos >= intervalMs * 1_000_000L;
	}

	void mark(long nowNanos) {
		taken = true;
		lastNanos = nowNanos;
	}
}
