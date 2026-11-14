package io.github.schrumbo.manycraft;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import net.minecraft.client.CloudStatus;
import net.minecraft.server.level.ParticleStatus;
import org.junit.jupiter.api.Test;

class BackgroundLevelTest {
	private static final BackgroundController.Values SAVED = new BackgroundController.Values(12, 10, ParticleStatus.ALL, CloudStatus.FANCY, 0.7, 120);

	@Test
	void savingCutsEverything() {
		assertEquals(new BackgroundController.Values(2, 5, ParticleStatus.MINIMAL, CloudStatus.OFF, 0.0, 120), SAVED.reduced(BackgroundLevel.SAVING, 2, 5, 10));
		assertTrue(BackgroundLevel.SAVING.skipsRender());
	}

	@Test
	void balancedKeepsSightAndCutsParticles() {
		assertEquals(new BackgroundController.Values(12, 10, ParticleStatus.MINIMAL, CloudStatus.FANCY, 0.0, 120), SAVED.reduced(BackgroundLevel.BALANCED, 2, 5, 10));
		assertFalse(BackgroundLevel.BALANCED.skipsRender());
	}

	@Test
	void fullOnlyMutes() {
		assertEquals(new BackgroundController.Values(12, 10, ParticleStatus.ALL, CloudStatus.FANCY, 0.0, 120), SAVED.reduced(BackgroundLevel.FULL, 2, 5, 10));
		assertFalse(BackgroundLevel.FULL.skipsRender());
	}

	@Test
	void customSetsSightAndFrameLimitAndCutsTheRest() {
		assertEquals(new BackgroundController.Values(6, 5, ParticleStatus.MINIMAL, CloudStatus.OFF, 0.0, 10), SAVED.reduced(BackgroundLevel.CUSTOM, 6, 5, 10));
		assertFalse(BackgroundLevel.CUSTOM.skipsRender());
	}

	@Test
	void customNeverGoesAboveThePlayersOwnValues() {
		BackgroundController.Values modest = new BackgroundController.Values(4, 4, ParticleStatus.ALL, CloudStatus.FAST, 1.0, 30);
		assertEquals(new BackgroundController.Values(4, 5, ParticleStatus.MINIMAL, CloudStatus.OFF, 0.0, 30), modest.reduced(BackgroundLevel.CUSTOM, 12, 5, 60));
	}

	@Test
	void optionFramerateStaysOnTheGamesSteps() {
		assertEquals(10, BackgroundLevel.optionFramerate(1));
		assertEquals(10, BackgroundLevel.optionFramerate(5));
		assertEquals(10, BackgroundLevel.optionFramerate(10));
		assertEquals(50, BackgroundLevel.optionFramerate(45));
		assertEquals(60, BackgroundLevel.optionFramerate(60));
		assertEquals(260, BackgroundLevel.optionFramerate(1000));
	}

	@Test
	void customValuesFallBackAndClamp() {
		assertEquals(new BackgroundLevel.Custom(5, 4), BackgroundLevel.Custom.of(null, null));
		assertEquals(new BackgroundLevel.Custom(10, 6), BackgroundLevel.Custom.of(10, 6));
		assertEquals(new BackgroundLevel.Custom(1, 2), BackgroundLevel.Custom.of(0, -1));
		assertEquals(new BackgroundLevel.Custom(260, 64), BackgroundLevel.Custom.of(999, 64));
		assertEquals(BackgroundLevel.CUSTOM, BackgroundLevel.parse("custom").orElseThrow());
	}

	@Test
	void customHidesWithGuardOnAndRestoresTheUsersValues() {
		SaveGuard guard = new SaveGuard();
		BackgroundController.Values[] live = {SAVED};
		BackgroundMode<BackgroundController.Values> mode = new BackgroundMode<>(new BackgroundMode.Target<>() {
			@Override
			public BackgroundController.Values read() {
				return live[0];
			}

			@Override
			public BackgroundController.Values background(BackgroundController.Values saved) {
				return saved.reduced(BackgroundLevel.CUSTOM, 6, 5, BackgroundLevel.optionFramerate(10));
			}

			@Override
			public void write(BackgroundController.Values values) {
				assertTrue(guard.blocksSave());
				live[0] = values;
			}
		}, guard);
		mode.enter();
		assertEquals(new BackgroundController.Values(6, 5, ParticleStatus.MINIMAL, CloudStatus.OFF, 0.0, 10), live[0]);
		mode.enter();
		mode.exit();
		assertEquals(SAVED, live[0]);
		assertFalse(guard.blocksSave());
	}
}
