package io.github.schrumbo.manycraft;

import com.sun.management.HotSpotDiagnosticMXBean;
import java.lang.management.ManagementFactory;
import java.lang.reflect.Field;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class HiddenMemory {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	static final int HIDDEN_WORKERS = 1;
	static final String MIN_FREE = "MinHeapFreeRatio";
	static final String MAX_FREE = "MaxHeapFreeRatio";
	static final int HIDDEN_MIN_FREE = 10;
	static final int HIDDEN_MAX_FREE = 25;

	interface Workers {
		int get();

		void set(int count);
	}

	interface Heap {
		int get(String flag);

		void set(String flag, int value);

		void collect();
	}

	private final Workers workers;
	private final Heap heap;
	private Integer savedWorkers;
	private int[] savedFree;

	HiddenMemory(Workers workers, Heap heap) {
		this.workers = workers;
		this.heap = heap;
	}

	static HiddenMemory create() {
		return new HiddenMemory(new SodiumWorkers(), new JvmHeap());
	}

	boolean capWorkers(boolean cap) {
		try {
			if (cap && savedWorkers == null) {
				int now = workers.get();
				workers.set(HIDDEN_WORKERS);
				savedWorkers = now;
			} else if (!cap && savedWorkers != null) {
				workers.set(savedWorkers);
				savedWorkers = null;
				return true;
			}
		} catch (RuntimeException | LinkageError e) {
			LOGGER.debug("chunk builder threads unchanged: {}", e.toString());
		}
		return false;
	}

	void trim() {
		try {
			if (savedFree == null) {
				int min = heap.get(MIN_FREE);
				int max = heap.get(MAX_FREE);
				int lowMin = Math.min(HIDDEN_MIN_FREE, min);
				int lowMax = Math.max(lowMin, Math.min(HIDDEN_MAX_FREE, max));
				heap.set(MIN_FREE, lowMin);
				savedFree = new int[] {min, max};
				heap.set(MAX_FREE, lowMax);
			}
			heap.collect();
		} catch (RuntimeException | LinkageError e) {
			LOGGER.debug("heap not trimmed: {}", e.toString());
		}
	}

	boolean restore() {
		boolean released = capWorkers(false);
		int[] saved = savedFree;
		if (saved == null) {
			return released;
		}
		savedFree = null;
		try {
			heap.set(MAX_FREE, saved[1]);
			heap.set(MIN_FREE, saved[0]);
		} catch (RuntimeException | LinkageError e) {
			LOGGER.debug("heap ratios not restored: {}", e.toString());
		}
		return released;
	}

	private static final class SodiumWorkers implements Workers {
		private Field threads;
		private Object performance;

		private Field field() {
			if (threads == null) {
				try {
					Class<?> mod = Class.forName("net.caffeinemc.mods.sodium.client.SodiumClientMod", false, HiddenMemory.class.getClassLoader());
					Object options = mod.getMethod("options").invoke(null);
					performance = options.getClass().getField("performance").get(options);
					threads = performance.getClass().getField("chunkBuilderThreads");
				} catch (ReflectiveOperationException | LinkageError e) {
					throw new IllegalStateException("no Sodium", e);
				}
			}
			return threads;
		}

		@Override
		public int get() {
			try {
				return field().getInt(performance);
			} catch (IllegalAccessException e) {
				throw new IllegalStateException(e);
			}
		}

		@Override
		public void set(int count) {
			try {
				field().setInt(performance, count);
			} catch (IllegalAccessException e) {
				throw new IllegalStateException(e);
			}
		}
	}

	private static final class JvmHeap implements Heap {
		private HotSpotDiagnosticMXBean bean;

		private HotSpotDiagnosticMXBean bean() {
			if (bean == null) {
				bean = ManagementFactory.getPlatformMXBean(HotSpotDiagnosticMXBean.class);
			}
			return bean;
		}

		@Override
		public int get(String flag) {
			return Integer.parseInt(bean().getVMOption(flag).getValue());
		}

		@Override
		public void set(String flag, int value) {
			bean().setVMOption(flag, Integer.toString(value));
		}

		@Override
		public void collect() {
			System.gc();
		}
	}
}
