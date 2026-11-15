package io.github.schrumbo.manycraft;

import java.util.function.UnaryOperator;

final class BackgroundMode<V> {
	interface Target<V> {
		V read();

		V background(V saved);

		void write(V values);
	}

	private final Target<V> target;
	private final SaveGuard guard;
	private V saved;

	BackgroundMode(Target<V> target, SaveGuard guard) {
		this.target = target;
		this.guard = guard;
	}

	boolean active() {
		return saved != null;
	}

	V saved() {
		return saved;
	}

	void enter() {
		if (saved == null) {
			guard.enable();
			saved = target.read();
		}
		target.write(target.background(saved));
	}

	void store(UnaryOperator<V> update) {
		if (saved == null) {
			return;
		}
		saved = update.apply(saved);
		target.write(target.background(saved));
	}

	void exit() {
		if (saved == null) {
			return;
		}
		V restore = saved;
		target.write(restore);
		saved = null;
		guard.disable();
	}
}
