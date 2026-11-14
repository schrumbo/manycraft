package io.github.schrumbo.manycraft;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

class WindowStepsTest {
	@Test
	void hiddenMaximizedGameLeavesMaximizedBeforeANormalPlace() {
		assertEquals(new WindowSteps(true, true), WindowSteps.of(true, true, "normal"));
		assertEquals(new WindowSteps(true, true), WindowSteps.of(true, true, "fullscreen"));
	}

	@Test
	void hiddenMaximizedGameStaysMaximizedForAMaximizedPlace() {
		assertEquals(new WindowSteps(false, true), WindowSteps.of(true, true, "maximized"));
	}

	@Test
	void hiddenGameDropsRestoredStateItWouldReapplyOnShow() {
		assertEquals(new WindowSteps(false, true), WindowSteps.of(true, false, "normal"));
	}

	@Test
	void shownGameIsOnlyRestoredWhenMaximized() {
		assertEquals(new WindowSteps(false, true), WindowSteps.of(false, true, "normal"));
		assertEquals(new WindowSteps(false, false), WindowSteps.of(false, false, "normal"));
		assertEquals(new WindowSteps(false, false), WindowSteps.of(false, false, "maximized"));
	}
}
