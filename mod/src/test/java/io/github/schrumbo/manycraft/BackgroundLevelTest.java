package io.github.schrumbo.manycraft;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import net.minecraft.client.CloudStatus;
import net.minecraft.server.level.ParticleStatus;
import org.junit.jupiter.api.Test;

class BackgroundLevelTest {
	private static final BackgroundController.Values SAVED = new BackgroundController.Values(12, 10, ParticleStatus.ALL, CloudStatus.FANCY, 0.7);

	@Test
	void savingCutsEverything() {
		assertEquals(new BackgroundController.Values(2, 5, ParticleStatus.MINIMAL, CloudStatus.OFF, 0.0), SAVED.reduced(BackgroundLevel.SAVING, 2, 5));
		assertTrue(BackgroundLevel.SAVING.skipsRender());
	}

	@Test
	void balancedKeepsSightAndCutsParticles() {
		assertEquals(new BackgroundController.Values(12, 10, ParticleStatus.MINIMAL, CloudStatus.FANCY, 0.0), SAVED.reduced(BackgroundLevel.BALANCED, 2, 5));
		assertFalse(BackgroundLevel.BALANCED.skipsRender());
	}

	@Test
	void fullOnlyMutes() {
		assertEquals(new BackgroundController.Values(12, 10, ParticleStatus.ALL, CloudStatus.FANCY, 0.0), SAVED.reduced(BackgroundLevel.FULL, 2, 5));
		assertFalse(BackgroundLevel.FULL.skipsRender());
	}
}
