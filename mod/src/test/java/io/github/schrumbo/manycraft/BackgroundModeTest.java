package io.github.schrumbo.manycraft;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.ArrayList;
import java.util.List;
import org.junit.jupiter.api.Test;

class BackgroundModeTest {
	record Opts(int rd, double volume) {}

	static final class Fake implements BackgroundMode.Target<Opts> {
		Opts live = new Opts(16, 0.8);
		final List<Boolean> guardAtWrite = new ArrayList<>();
		final SaveGuard guard;

		Fake(SaveGuard guard) {
			this.guard = guard;
		}

		@Override
		public Opts read() {
			return live;
		}

		@Override
		public Opts background(Opts saved) {
			return new Opts(2, 0.0);
		}

		@Override
		public void write(Opts values) {
			guardAtWrite.add(guard.blocksSave());
			live = values;
		}
	}

	@Test
	void guardBlocksSavesOnlyWhileEnabled() {
		SaveGuard guard = new SaveGuard();
		assertFalse(guard.blocksSave());
		guard.enable();
		assertTrue(guard.blocksSave());
		guard.disable();
		assertFalse(guard.blocksSave());
	}

	@Test
	void enterAppliesBackgroundWithGuardAlreadyOn() {
		SaveGuard guard = new SaveGuard();
		Fake fake = new Fake(guard);
		BackgroundMode<Opts> mode = new BackgroundMode<>(fake, guard);
		mode.enter();
		assertEquals(new Opts(2, 0.0), fake.live);
		assertEquals(List.of(true), fake.guardAtWrite);
		assertTrue(mode.active());
		assertTrue(guard.blocksSave());
	}

	@Test
	void exitRestoresExactValuesBeforeLiftingGuard() {
		SaveGuard guard = new SaveGuard();
		Fake fake = new Fake(guard);
		BackgroundMode<Opts> mode = new BackgroundMode<>(fake, guard);
		mode.enter();
		mode.exit();
		assertEquals(new Opts(16, 0.8), fake.live);
		assertEquals(List.of(true, true), fake.guardAtWrite);
		assertFalse(mode.active());
		assertFalse(guard.blocksSave());
	}

	@Test
	void reenterKeepsOriginalSnapshot() {
		SaveGuard guard = new SaveGuard();
		Fake fake = new Fake(guard);
		BackgroundMode<Opts> mode = new BackgroundMode<>(fake, guard);
		mode.enter();
		mode.enter();
		mode.exit();
		assertEquals(new Opts(16, 0.8), fake.live);
	}

	@Test
	void exitWithoutEnterDoesNothing() {
		SaveGuard guard = new SaveGuard();
		Fake fake = new Fake(guard);
		BackgroundMode<Opts> mode = new BackgroundMode<>(fake, guard);
		mode.exit();
		assertTrue(fake.guardAtWrite.isEmpty());
		assertEquals(new Opts(16, 0.8), fake.live);
		assertFalse(guard.blocksSave());
	}

	@Test
	void secondCycleSnapshotsFreshValues() {
		SaveGuard guard = new SaveGuard();
		Fake fake = new Fake(guard);
		BackgroundMode<Opts> mode = new BackgroundMode<>(fake, guard);
		mode.enter();
		mode.exit();
		fake.live = new Opts(8, 0.3);
		mode.enter();
		mode.exit();
		assertEquals(new Opts(8, 0.3), fake.live);
	}

	@Test
	void adoptUpdatesThePlayersValuesAndKeepsTheBackgroundInForce() {
		SaveGuard guard = new SaveGuard();
		Fake fake = new Fake(guard);
		BackgroundMode<Opts> mode = new BackgroundMode<>(fake, guard);
		mode.enter();
		fake.live = new Opts(24, 0.0);
		mode.adopt((saved, live) -> new Opts(live.rd(), saved.volume()));
		assertEquals(new Opts(2, 0.0), fake.live);
		assertTrue(guard.blocksSave());
		mode.exit();
		assertEquals(new Opts(24, 0.8), fake.live);
	}

	@Test
	void adoptWhileShownLeavesTheLiveValues() {
		SaveGuard guard = new SaveGuard();
		Fake fake = new Fake(guard);
		BackgroundMode<Opts> mode = new BackgroundMode<>(fake, guard);
		fake.live = new Opts(24, 0.5);
		mode.adopt((saved, live) -> new Opts(1, 1.0));
		assertEquals(new Opts(24, 0.5), fake.live);
		assertTrue(fake.guardAtWrite.isEmpty());
	}
}
