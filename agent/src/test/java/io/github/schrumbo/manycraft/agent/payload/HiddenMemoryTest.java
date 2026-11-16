package io.github.schrumbo.manycraft.agent.payload;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

class HiddenMemoryTest {
	static final class Workers implements HiddenMemory.Workers {
		int count;
		boolean broken;

		Workers(int count) {
			this.count = count;
		}

		@Override
		public int get() {
			if (broken) {
				throw new IllegalStateException("no Sodium");
			}
			return count;
		}

		@Override
		public void set(int value) {
			if (broken) {
				throw new IllegalStateException("no Sodium");
			}
			count = value;
		}
	}

	static final class Heap implements HiddenMemory.Heap {
		final Map<String, Integer> flags = new HashMap<>();
		final List<String> log = new ArrayList<>();
		int collections;
		boolean locked;

		Heap(int min, int max) {
			flags.put(HiddenMemory.MIN_FREE, min);
			flags.put(HiddenMemory.MAX_FREE, max);
		}

		@Override
		public int get(String flag) {
			return flags.get(flag);
		}

		@Override
		public void set(String flag, int value) {
			if (locked) {
				throw new IllegalArgumentException("not writeable");
			}
			flags.put(flag, value);
			log.add(flag + "=" + value);
			assertTrue(flags.get(HiddenMemory.MIN_FREE) <= flags.get(HiddenMemory.MAX_FREE), log.toString());
		}

		@Override
		public void collect() {
			collections++;
		}
	}

	@Test
	void capsWorkersOnceAndRestoresTheUsersValue() {
		Workers workers = new Workers(0);
		HiddenMemory memory = new HiddenMemory(workers, new Heap(40, 70));
		assertFalse(memory.capWorkers(true));
		assertEquals(HiddenMemory.HIDDEN_WORKERS, workers.count);
		assertFalse(memory.capWorkers(true));
		assertTrue(memory.capWorkers(false));
		assertEquals(0, workers.count);
		assertFalse(memory.capWorkers(false));
		assertEquals(0, workers.count);
	}

	@Test
	void restoreAlsoReleasesTheWorkerCap() {
		Workers workers = new Workers(6);
		HiddenMemory memory = new HiddenMemory(workers, new Heap(40, 70));
		memory.capWorkers(true);
		memory.trim();
		assertTrue(memory.restore());
		assertEquals(6, workers.count);
		assertFalse(memory.restore());
	}

	@Test
	void withoutSodiumNothingHappens() {
		Workers workers = new Workers(3);
		workers.broken = true;
		HiddenMemory memory = new HiddenMemory(workers, new Heap(40, 70));
		memory.capWorkers(true);
		workers.broken = false;
		assertFalse(memory.capWorkers(false));
		assertEquals(3, workers.count);
	}

	@Test
	void trimLowersRatiosKeepingMinBelowMaxAndCollects() {
		Heap heap = new Heap(40, 70);
		HiddenMemory memory = new HiddenMemory(new Workers(0), heap);
		memory.trim();
		assertEquals(HiddenMemory.HIDDEN_MIN_FREE, heap.get(HiddenMemory.MIN_FREE));
		assertEquals(HiddenMemory.HIDDEN_MAX_FREE, heap.get(HiddenMemory.MAX_FREE));
		assertEquals(1, heap.collections);
		memory.trim();
		assertEquals(2, heap.collections);
		memory.restore();
		assertEquals(40, heap.get(HiddenMemory.MIN_FREE));
		assertEquals(70, heap.get(HiddenMemory.MAX_FREE));
		memory.restore();
		assertEquals(List.of("MinHeapFreeRatio=10", "MaxHeapFreeRatio=25", "MaxHeapFreeRatio=70", "MinHeapFreeRatio=40"), heap.log);
	}

	@Test
	void trimNeverRaisesRatiosTheUserSetLower() {
		Heap heap = new Heap(5, 8);
		HiddenMemory memory = new HiddenMemory(new Workers(0), heap);
		memory.trim();
		assertEquals(5, heap.get(HiddenMemory.MIN_FREE));
		assertEquals(8, heap.get(HiddenMemory.MAX_FREE));
		memory.restore();
		assertEquals(5, heap.get(HiddenMemory.MIN_FREE));
		assertEquals(8, heap.get(HiddenMemory.MAX_FREE));
	}

	@Test
	void lockedFlagsChangeNothing() {
		Heap heap = new Heap(40, 70);
		heap.locked = true;
		HiddenMemory memory = new HiddenMemory(new Workers(0), heap);
		memory.trim();
		assertEquals(0, heap.collections);
		heap.locked = false;
		memory.restore();
		assertEquals(List.of(), heap.log);
	}
}
