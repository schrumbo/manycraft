package io.github.schrumbo.manycraft;

import java.util.Optional;

enum BackgroundLevel {
	SAVING,
	BALANCED,
	FULL,
	CUSTOM;

	static final int BALANCED_FPS = 30;
	static final int SKIP_DRAW_MAX_FPS = 10;
	static final int OPTION_FPS_STEP = 10;
	static final int OPTION_FPS_MAX = 260;

	static Optional<BackgroundLevel> parse(String wire) {
		if (wire == null) {
			return Optional.empty();
		}
		return switch (wire) {
			case "saving" -> Optional.of(SAVING);
			case "balanced" -> Optional.of(BALANCED);
			case "full" -> Optional.of(FULL);
			case "custom" -> Optional.of(CUSTOM);
			default -> Optional.empty();
		};
	}

	boolean skipsRender(Custom custom) {
		return this == SAVING || this == CUSTOM && custom.fps() <= SKIP_DRAW_MAX_FPS;
	}

	static int optionFramerate(int fps) {
		int stepped = (fps + OPTION_FPS_STEP - 1) / OPTION_FPS_STEP * OPTION_FPS_STEP;
		return Math.clamp(stepped, OPTION_FPS_STEP, OPTION_FPS_MAX);
	}

	record Custom(int fps, int renderDistance) {
		static final Custom DEFAULT = new Custom(5, 4);

		static Custom of(Integer fps, Integer renderDistance) {
			return new Custom(
				Math.clamp(fps == null ? DEFAULT.fps : fps, 1, OPTION_FPS_MAX),
				Math.max(2, renderDistance == null ? DEFAULT.renderDistance : renderDistance)
			);
		}
	}
}
