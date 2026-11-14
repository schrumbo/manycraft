package io.github.schrumbo.manycraft;

final class SaveGuard {
	private volatile boolean active;

	void enable() {
		active = true;
	}

	void disable() {
		active = false;
	}

	boolean blocksSave() {
		return active;
	}
}
