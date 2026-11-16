package io.github.schrumbo.manycraft.agent.payload;

final class WhenShown {
	private Runnable pending;

	void run(boolean hidden, Runnable action) {
		if (hidden) {
			pending = action;
			return;
		}
		pending = null;
		action.run();
	}

	void shown() {
		Runnable action = pending;
		pending = null;
		if (action != null) {
			action.run();
		}
	}
}
