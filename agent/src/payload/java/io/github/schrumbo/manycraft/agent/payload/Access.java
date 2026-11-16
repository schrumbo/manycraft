package io.github.schrumbo.manycraft.agent.payload;

import io.github.schrumbo.manycraft.agent.Agent;
import java.lang.reflect.Field;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class Access {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");

	private Access() {}

	static Field field(Class<?> owner, String name) {
		Field f = optional(owner, name);
		if (f == null) {
			LOGGER.warn("{}.{} not found, the companion leaves it alone", owner.getSimpleName(), name);
		}
		return f;
	}

	static Field optional(Class<?> owner, String name) {
		try {
			Field f = owner.getDeclaredField(name);
			Agent.open(owner, Access.class.getModule());
			f.setAccessible(true);
			return f;
		} catch (ReflectiveOperationException | RuntimeException | LinkageError e) {
			return null;
		}
	}

	@SuppressWarnings("unchecked")
	static <T> T get(Field field, Object target) {
		if (field == null) {
			return null;
		}
		try {
			return (T) field.get(target);
		} catch (IllegalAccessException e) {
			return null;
		}
	}

	static boolean set(Field field, Object target, Object value) {
		if (field == null) {
			return false;
		}
		try {
			field.set(target, value);
			return true;
		} catch (IllegalAccessException | IllegalArgumentException e) {
			return false;
		}
	}
}
