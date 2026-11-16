package io.github.schrumbo.manycraft.agent;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.lang.reflect.Proxy;
import java.util.ArrayList;
import java.util.List;
import java.util.function.Function;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.Test;

class HooksTest {
	@AfterEach
	void reset() {
		Hooks.bridge = null;
		Hooks.guarded = false;
		System.clearProperty("manycraft.ipc");
		System.clearProperty("manycraft.proxy");
	}

	private static Bridge bridge(Function<Object[], Object> save) {
		return (Bridge) Proxy.newProxyInstance(Bridge.class.getClassLoader(), new Class<?>[] {Bridge.class}, (proxy, method, args) -> {
			if (method.getName().equals("saveOptions")) {
				return save.apply(args);
			}
			throw new UnsupportedOperationException(method.getName());
		});
	}

	@Test
	void withoutTheCompanionTheGameSavesItself() {
		assertFalse(Hooks.save(new Object()));
	}

	@Test
	void theCompanionDecidesEverySave() {
		List<Object> seen = new ArrayList<>();
		Hooks.bridge = bridge(args -> {
			seen.add(args[0]);
			return true;
		});
		Object options = new Object();
		assertTrue(Hooks.save(options));
		assertEquals(List.of(options), seen);
	}

	@Test
	void theOriginalSaveRunsThroughUnhooked() {
		Hooks.bridge = bridge(args -> true);
		List<Boolean> inner = new ArrayList<>();
		Hooks.saveOriginal(() -> inner.add(Hooks.save(new Object())));
		assertEquals(List.of(false), inner);
		assertTrue(Hooks.save(new Object()));
	}

	@Test
	void aFailingCompanionNeverWritesBackgroundOptions() {
		Hooks.bridge = bridge(args -> {
			throw new IllegalStateException("broken");
		});
		Hooks.guarded = true;
		assertTrue(Hooks.save(new Object()));
		Hooks.guarded = false;
		assertFalse(Hooks.save(new Object()));
	}

	@Test
	void aProxiedConnectionWithoutTheCompanionFailsClosed() {
		System.setProperty("manycraft.ipc", "127.0.0.1:4000");
		System.setProperty("manycraft.proxy", "socks5://127.0.0.1:1080");
		assertThrows(IllegalStateException.class, () -> Hooks.channel(new Object()));
		System.clearProperty("manycraft.proxy");
		Hooks.channel(new Object());
	}
}
