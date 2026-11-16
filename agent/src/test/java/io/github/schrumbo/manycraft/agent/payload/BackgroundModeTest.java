package io.github.schrumbo.manycraft.agent.payload;

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
		final List<Opts> written = new ArrayList<>();
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
			written.add(values);
			live = values;
		}
	}

	@Test
	void guardBlocksSavesOnlyWhileEnabled() {
		SaveGuard guard = new SaveGuard(() -> true, on -> {});
		assertFalse(guard.blocksSave());
		guard.enable();
		assertTrue(guard.blocksSave());
		guard.disable();
		assertFalse(guard.blocksSave());
	}

	@Test
	void withoutTheSaveHookBackgroundOptionsAreNeverApplied() {
		SaveGuard guard = new SaveGuard(() -> false, on -> {});
		Fake fake = new Fake(guard);
		BackgroundMode<Opts> mode = new BackgroundMode<>(fake, guard);
		mode.enter();
		assertEquals(new Opts(16, 0.8), fake.live);
		assertTrue(fake.written.isEmpty());
		assertFalse(mode.active());
		assertFalse(guard.blocksSave());
	}

	@Test
	void theGuardMirrorsItsStateForTheBootHook() {
		List<Boolean> mirrored = new ArrayList<>();
		SaveGuard guard = new SaveGuard(() -> true, mirrored::add);
		BackgroundMode<Opts> mode = new BackgroundMode<>(new Fake(guard), guard);
		mode.enter();
		mode.exit();
		assertEquals(List.of(true, false), mirrored);
	}

	@Test
	void enterAppliesBackgroundWithGuardAlreadyOn() {
		SaveGuard guard = new SaveGuard(() -> true, on -> {});
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
		SaveGuard guard = new SaveGuard(() -> true, on -> {});
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
		SaveGuard guard = new SaveGuard(() -> true, on -> {});
		Fake fake = new Fake(guard);
		BackgroundMode<Opts> mode = new BackgroundMode<>(fake, guard);
		mode.enter();
		mode.enter();
		mode.exit();
		assertEquals(new Opts(16, 0.8), fake.live);
	}

	@Test
	void exitWithoutEnterDoesNothing() {
		SaveGuard guard = new SaveGuard(() -> true, on -> {});
		Fake fake = new Fake(guard);
		BackgroundMode<Opts> mode = new BackgroundMode<>(fake, guard);
		mode.exit();
		assertTrue(fake.guardAtWrite.isEmpty());
		assertEquals(new Opts(16, 0.8), fake.live);
		assertFalse(guard.blocksSave());
	}

	@Test
	void secondCycleSnapshotsFreshValues() {
		SaveGuard guard = new SaveGuard(() -> true, on -> {});
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
	void aStoredValueWhileHiddenNeverReachesTheLiveOptions() {
		SaveGuard guard = new SaveGuard(() -> true, on -> {});
		Fake fake = new Fake(guard);
		BackgroundMode<Opts> mode = new BackgroundMode<>(fake, guard);
		mode.enter();
		mode.store(saved -> new Opts(24, saved.volume()));
		assertEquals(new Opts(2, 0.0), fake.live);
		assertTrue(fake.written.stream().allMatch(o -> o.equals(new Opts(2, 0.0))));
		assertTrue(guard.blocksSave());
		mode.exit();
		assertEquals(new Opts(24, 0.8), fake.live);
	}

	@Test
	void storeWhileShownLeavesTheLiveValues() {
		SaveGuard guard = new SaveGuard(() -> true, on -> {});
		Fake fake = new Fake(guard);
		BackgroundMode<Opts> mode = new BackgroundMode<>(fake, guard);
		fake.live = new Opts(24, 0.5);
		mode.store(saved -> new Opts(1, 1.0));
		assertEquals(new Opts(24, 0.5), fake.live);
		assertTrue(fake.guardAtWrite.isEmpty());
	}

	@Test
	void aPackReloadWhileHiddenWaitsForTheShowAndRunsOnce() {
		List<String> runs = new ArrayList<>();
		WhenShown queue = new WhenShown();
		queue.run(true, () -> runs.add("first"));
		queue.run(true, () -> runs.add("second"));
		assertTrue(runs.isEmpty());
		queue.shown();
		assertEquals(List.of("second"), runs);
		queue.shown();
		assertEquals(List.of("second"), runs);
		queue.run(false, () -> runs.add("now"));
		assertEquals(List.of("second", "now"), runs);
	}

	@Test
	void aReloadWhileShownDropsTheQueuedOne() {
		List<String> runs = new ArrayList<>();
		WhenShown queue = new WhenShown();
		queue.run(true, () -> runs.add("queued"));
		queue.run(false, () -> runs.add("now"));
		queue.shown();
		assertEquals(List.of("now"), runs);
	}
}
