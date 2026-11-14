package io.github.schrumbo.manycraft;

import java.util.Optional;

enum BackgroundLevel {
	SAVING,
	BALANCED,
	FULL;

	static final int BALANCED_FPS = 30;

	static Optional<BackgroundLevel> parse(String wire) {
		if (wire == null) {
			return Optional.empty();
		}
		return switch (wire) {
			case "saving" -> Optional.of(SAVING);
			case "balanced" -> Optional.of(BALANCED);
			case "full" -> Optional.of(FULL);
			default -> Optional.empty();
		};
	}

	boolean skipsRender() {
		return this == SAVING;
	}
}
