package io.github.schrumbo.manycraft.agent.payload;

import java.util.function.BooleanSupplier;
import java.util.function.Consumer;

final class SaveGuard {
	private final BooleanSupplier hooked;
	private final Consumer<Boolean> mirror;
	private volatile boolean active;

	SaveGuard(BooleanSupplier hooked, Consumer<Boolean> mirror) {
		this.hooked = hooked;
		this.mirror = mirror;
	}

	boolean available() {
		return hooked.getAsBoolean();
	}

	void enable() {
		active = true;
		mirror.accept(true);
	}

	void disable() {
		active = false;
		mirror.accept(false);
	}

	boolean blocksSave() {
		return active;
	}
}
