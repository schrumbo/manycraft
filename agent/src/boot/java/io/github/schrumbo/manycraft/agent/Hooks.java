package io.github.schrumbo.manycraft.agent;

import java.io.IOException;
import java.io.UncheckedIOException;
import java.net.Socket;
import java.net.SocketAddress;
import java.net.http.HttpClient;

/**
 * What patched game code calls. Lives on the boot class path so every loader sees it and forwards to the payload; every
 * hook leaves the game alone when the payload is missing or fails, except that a requested proxy is never bypassed.
 */
public final class Hooks {
	static volatile Bridge bridge;
	static volatile boolean failed;
	/**
	 * Set by the payload while background options are applied; a failing save hook then skips the save.
	 */
	public static volatile boolean guarded;
	private static final ThreadLocal<Boolean> SAVING = new ThreadLocal<>();

	private Hooks() {}

	public static boolean startHidden() {
		return Boolean.getBoolean("manycraft.hidden") && !failed && Patches.has(Patches.CORE);
	}

	public static long windowFlags(long flags) {
		return startHidden() ? flags | 8L : flags;
	}

	public static void frame(Object minecraft) {
		Bridge b = bridge;
		if (b == null) {
			if (failed) {
				return;
			}
			Agent.ensure(minecraft);
			b = bridge;
			if (b == null) {
				return;
			}
		}
		try {
			b.frame(minecraft);
		} catch (Throwable e) {
			Agent.fault("frame", e);
		}
	}

	/**
	 * Whether the headless prototype is on: a hidden game then skips whole frames instead of only the world render.
	 */
	public static boolean headless() {
		return Boolean.getBoolean("manycraft.headless");
	}

	/**
	 * Runs {@link #frame} and tells whether the rest of the frame is skipped.
	 */
	public static boolean frameOrSkip(Object minecraft) {
		frame(minecraft);
		Bridge b = bridge;
		if (b == null) {
			return false;
		}
		try {
			return b.skipFrame(minecraft);
		} catch (Throwable e) {
			Agent.fault("headless", e);
			return false;
		}
	}

	public static void tick(Object minecraft) {
		Bridge b = bridge;
		if (b == null) {
			return;
		}
		try {
			b.tick(minecraft);
		} catch (Throwable e) {
			Agent.fault("tick", e);
		}
	}

	public static boolean beforeRender(Object renderer) {
		Bridge b = bridge;
		if (b == null) {
			return true;
		}
		try {
			return b.beforeRender(renderer);
		} catch (Throwable e) {
			Agent.fault("render", e);
			return true;
		}
	}

	public static void afterRender(Object renderer) {
		Bridge b = bridge;
		if (b == null) {
			return;
		}
		try {
			b.afterRender(renderer);
		} catch (Throwable e) {
			Agent.fault("preview", e);
		}
	}

	public static String title() {
		Bridge b = bridge;
		if (b == null) {
			return null;
		}
		try {
			return b.title();
		} catch (Throwable e) {
			Agent.fault("title", e);
			return null;
		}
	}

	public static boolean managed() {
		Bridge b = bridge;
		try {
			return b != null && b.managed();
		} catch (Throwable e) {
			Agent.fault("pause", e);
			return false;
		}
	}

	public static int fps() {
		Bridge b = bridge;
		try {
			return b == null ? 0 : b.hiddenFramerate();
		} catch (Throwable e) {
			Agent.fault("fps", e);
			return 0;
		}
	}

	public static void audioClosed() {
		Bridge b = bridge;
		if (b == null) {
			return;
		}
		try {
			b.audioClosed();
		} catch (Throwable e) {
			Agent.fault("audio", e);
		}
	}

	/**
	 * Called first in {@code Options.save}; true means the payload handled the save and the original body is skipped.
	 * The payload runs the original through {@link #saveOriginal}.
	 */
	public static boolean save(Object options) {
		Bridge b = bridge;
		if (b == null || SAVING.get() != null) {
			return false;
		}
		try {
			return b.saveOptions(options);
		} catch (Throwable e) {
			Agent.fault("options", e);
			return guarded;
		}
	}

	/**
	 * Runs {@code save} with the save hook passing through to the original {@code Options.save} body.
	 */
	public static void saveOriginal(Runnable save) {
		SAVING.set(Boolean.TRUE);
		try {
			save.run();
		} finally {
			SAVING.remove();
		}
	}

	public static void multiplayerOpened(Object screen) {
		Bridge b = bridge;
		if (b == null) {
			return;
		}
		try {
			b.multiplayerOpened(screen);
		} catch (Throwable e) {
			Agent.fault("servers", e);
		}
	}

	public static void serversSaved() {
		Bridge b = bridge;
		if (b == null) {
			return;
		}
		try {
			b.serversSaved();
		} catch (Throwable e) {
			Agent.fault("servers", e);
		}
	}

	public static void closeRequested() {
		Bridge b = bridge;
		if (b == null) {
			return;
		}
		try {
			b.closeRequested();
		} catch (Throwable e) {
			Agent.fault("close", e);
		}
	}

	public static boolean closing(boolean closing) {
		if (closing) {
			closeRequested();
		}
		return closing;
	}

	public static boolean key(int action, Object event) {
		Bridge b = bridge;
		try {
			return b != null && b.key(action, event);
		} catch (Throwable e) {
			Agent.fault("keys", e);
			return false;
		}
	}

	public static void connecting(Object serverData) {
		Bridge b = bridge;
		if (b == null) {
			return;
		}
		try {
			b.connecting(serverData);
		} catch (Throwable e) {
			Agent.fault("connecting", e);
		}
	}

	/**
	 * Puts the proxy in front of a new client channel. Throws when a proxy is requested but cannot be applied, so the
	 * connection fails instead of going direct.
	 */
	public static void channel(Object channel) {
		if (!Proxies.requested()) {
			return;
		}
		Bridge b = bridge;
		if (b == null) {
			throw new IllegalStateException("proxy requested but the companion is not ready, refusing a direct connection");
		}
		b.channel(channel);
	}

	public static void ping(Object socket, Object address, int timeout) throws IOException {
		if (Proxies.requested()) {
			throw new IOException("a proxy is set, Realms regions are not pinged directly");
		}
		((Socket) socket).connect((SocketAddress) address, timeout);
	}

	public static Object httpClient(Object builder) {
		if (Proxies.requested() && !Proxies.reachesHttpClients()) {
			throw new UncheckedIOException(new IOException("a SOCKS proxy is set and this transfer cannot use it, refused instead of going direct"));
		}
		return ((HttpClient.Builder) builder).build();
	}

	public static Object javaProxy(Object current) {
		Object ours = Proxies.javaProxy();
		return ours == null ? current : ours;
	}

	public static Object remoteName(Object address) {
		Bridge b = bridge;
		if (b == null || !Proxies.requested()) {
			return null;
		}
		try {
			return b.remoteName(address);
		} catch (Throwable e) {
			Agent.fault("dns", e);
			return null;
		}
	}

	public static void systemChat(Object message, boolean overlay) {
		Bridge b = bridge;
		if (b == null) {
			return;
		}
		try {
			b.systemChat(message, overlay);
		} catch (Throwable e) {
			Agent.fault("chat", e);
		}
	}

	public static void playerChat(Object bound, Object signed, Object decorated, Object sender, Object receivedAt) {
		Bridge b = bridge;
		if (b == null) {
			return;
		}
		try {
			b.playerChat(bound, signed, decorated, sender, receivedAt);
		} catch (Throwable e) {
			Agent.fault("chat", e);
		}
	}

	public static void hud(Object graphics, Object delta) {
		Bridge b = bridge;
		if (b == null) {
			return;
		}
		try {
			b.hud(graphics, delta);
		} catch (Throwable e) {
			Agent.fault("hud", e);
		}
	}
}
