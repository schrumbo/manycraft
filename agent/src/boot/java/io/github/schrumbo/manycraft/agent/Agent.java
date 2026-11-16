package io.github.schrumbo.manycraft.agent;

import java.io.IOException;
import java.io.InputStream;
import java.lang.instrument.Instrumentation;
import java.lang.reflect.Method;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Entry point of the Manycraft companion agent. Patches the game as its classes load and, on the first frame, loads the
 * payload next to the game's class loader. Does nothing unless the manager launched the game.
 */
public final class Agent {
	static final String PAYLOAD = "io.github.schrumbo.manycraft.agent.payload.Entry";
	private static final Set<String> FAULTS = ConcurrentHashMap.newKeySet();
	private static volatile Instrumentation inst;
	private static volatile boolean loaded;

	private Agent() {}

	/**
	 * Called by the JVM for {@code -javaagent}; installs the patcher when {@code manycraft.ipc} is set.
	 */
	public static void premain(String args, Instrumentation instrumentation) {
		if (System.getProperty("manycraft.ipc") == null) {
			return;
		}
		try {
			inst = instrumentation;
			Proxies.installDefaults();
			instrumentation.addTransformer(new Patcher(instrumentation));
		} catch (Throwable e) {
			log("not attached: " + e);
			if (Proxies.raw() != null) {
				log("a proxy is set, so the game must not start without the companion");
				Runtime.getRuntime().halt(1);
			}
		}
	}

	static void log(String line) {
		System.err.println("[manycraft-agent] " + line);
	}

	static void fault(String where, Throwable e) {
		if (FAULTS.add(where)) {
			log(where + " failed, left to the game: " + e);
		}
	}

	/**
	 * Opens the package of {@code target} to {@code to} so reflection works on loaders that put the game in a module.
	 */
	public static void open(Class<?> target, Module to) {
		Module module = target.getModule();
		Instrumentation i = inst;
		if (i == null || !module.isNamed() || module.isOpen(target.getPackageName(), to)) {
			return;
		}
		i.redefineModule(module, Set.of(), Map.of(), Map.of(target.getPackageName(), Set.of(to)), Set.of(), Map.of());
	}

	static void ensure(Object anchor) {
		if (loaded) {
			return;
		}
		synchronized (Agent.class) {
			if (loaded) {
				return;
			}
			loaded = true;
			if (Proxies.requested() && !proxyHooked(anchor.getClass().getClassLoader())) {
				log("a proxy is set but its hooks are missing " + Patches.applied() + ", stopping the game instead of connecting without it");
				Runtime.getRuntime().halt(1);
			}
			long start = System.nanoTime();
			try {
				ClassLoader loader = new PayloadLoader(anchor.getClass().getClassLoader());
				Method install = Class.forName(PAYLOAD, true, loader).getMethod("install", Map.class);
				Object bridge = install.invoke(null, Patches.applied());
				if (bridge instanceof Bridge b) {
					Hooks.bridge = b;
					log("companion ready in " + (System.nanoTime() - start) / 1_000_000 + " ms, hooks " + Patches.applied().keySet());
				} else {
					Hooks.failed = true;
					reveal(anchor);
				}
			} catch (Throwable e) {
				log("companion not loaded, game runs without it: " + e);
				Hooks.failed = true;
				reveal(anchor);
			}
		}
	}

	static boolean proxyHooked(ClassLoader game) {
		try {
			Class.forName("io.netty.bootstrap.Bootstrap", true, game);
		} catch (Throwable e) {
			log("netty not found: " + e);
			return false;
		}
		return Patches.has("proxy") && Patches.has("signin");
	}

	private static void reveal(Object minecraft) {
		if (!Boolean.getBoolean("manycraft.hidden")) {
			return;
		}
		try {
			ClassLoader loader = minecraft.getClass().getClassLoader();
			Object window = minecraft.getClass().getMethod("getWindow").invoke(minecraft);
			long handle = (long) window.getClass().getMethod("handle").invoke(window);
			try {
				Class.forName("org.lwjgl.sdl.SDLVideo", true, loader).getMethod("SDL_ShowWindow", long.class).invoke(null, handle);
			} catch (ClassNotFoundException e) {
				Class.forName("org.lwjgl.glfw.GLFW", true, loader).getMethod("glfwShowWindow", long.class).invoke(null, handle);
			}
		} catch (Throwable e) {
			log("cannot show the hidden window: " + e);
		}
	}

	static byte[] resource(String name) throws IOException {
		try (InputStream in = ClassLoader.getSystemResourceAsStream(name)) {
			if (in == null) {
				throw new IOException("missing " + name);
			}
			return in.readAllBytes();
		}
	}

	private static final class PayloadLoader extends ClassLoader {
		static {
			registerAsParallelCapable();
		}

		PayloadLoader(ClassLoader parent) {
			super("manycraft-agent", parent);
		}

		@Override
		protected Class<?> findClass(String name) throws ClassNotFoundException {
			if (!name.startsWith("io.github.schrumbo.manycraft.agent.payload.")) {
				throw new ClassNotFoundException(name);
			}
			try {
				byte[] bytes = resource("payload/" + name.replace('.', '/') + ".bin");
				return defineClass(name, bytes, 0, bytes.length);
			} catch (IOException e) {
				throw new ClassNotFoundException(name, e);
			}
		}
	}
}
