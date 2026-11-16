package io.github.schrumbo.manycraft.agent;

import java.util.Map;
import java.util.TreeMap;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Which hooks found their target in this game; the payload turns off what depends on a missing one.
 */
public final class Patches {
	public static final String CORE = "core";
	private static final Map<String, Boolean> APPLIED = new ConcurrentHashMap<>();

	private Patches() {}

	static void record(String feature, boolean applied) {
		APPLIED.merge(feature, applied, Boolean::logicalAnd);
	}

	public static boolean has(String feature) {
		if (CORE.equals(feature)) {
			return has("tick") && has("frame");
		}
		return APPLIED.getOrDefault(feature, false);
	}

	/**
	 * Every hook seen so far and whether it applied.
	 */
	public static Map<String, Boolean> applied() {
		return new TreeMap<>(APPLIED);
	}
}
