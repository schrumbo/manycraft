package io.github.schrumbo.manycraft;

import io.netty.channel.Channel;
import net.minecraft.client.Options;
import net.minecraft.client.gui.screens.multiplayer.JoinMultiplayerScreen;
import net.minecraft.client.multiplayer.ServerData;
import net.minecraft.client.renderer.GameRenderer;

/**
 * Entry points the mixins call; every method is a no-op without a running companion.
 */
public final class CompanionHooks {
	private CompanionHooks() {}

	public static boolean startHidden() {
		try {
			Config config = Config.read(System::getProperty, System::getenv);
			return config != null && config.hidden();
		} catch (RuntimeException e) {
			return false;
		}
	}

	public static boolean proxyRequested() {
		return ProxyInstaller.requested();
	}

	public static java.net.Proxy javaProxy() {
		ProxyInstaller.installDefaults();
		return ProxyInstaller.javaProxy();
	}

	public static java.util.Optional<java.net.InetSocketAddress> remoteName(String host, int port) {
		return ProxyInstaller.remoteName(host, port);
	}

	public static boolean managed() {
		return Companion.get() != null;
	}

	public static int hiddenFramerate() {
		Companion c = Companion.get();
		return c != null && c.hidden() ? c.hiddenFramerate() : 0;
	}

	public static void audioClosed() {
		Companion c = Companion.get();
		if (c != null) {
			c.audioClosed();
		}
	}

	public static String title() {
		Companion c = Companion.get();
		return c == null ? null : c.title();
	}

	public static boolean blocksSave() {
		Companion c = Companion.get();
		return c != null && c.blocksSave();
	}

	public static void saveOptions(Options options, Runnable save) {
		Runnable windowed = () -> GameWindow.saving(options, save);
		Companion c = Companion.get();
		if (c == null) {
			windowed.run();
		} else {
			c.saveOptions(options, windowed);
		}
	}

	public static void multiplayerOpened(JoinMultiplayerScreen screen) {
		Companion c = Companion.get();
		if (c != null) {
			c.multiplayerOpened(screen);
		}
	}

	public static void serversSaved() {
		Companion c = Companion.get();
		if (c != null) {
			c.serversSaved();
		}
	}

	public static boolean closeToGrid() {
		Companion c = Companion.get();
		return c != null && c.closeToGrid();
	}

	public static boolean onKey(int action, int keycode, int modifiers) {
		Companion c = Companion.get();
		return c != null && c.onKey(action, keycode, modifiers);
	}

	public static void render(GameRenderer renderer, Runnable body) {
		Companion c = Companion.get();
		if (c == null) {
			body.run();
		} else {
			c.render(renderer, body);
		}
	}

	public static void noteConnecting(ServerData data) {
		Companion c = Companion.get();
		if (c != null) {
			c.noteConnecting(data);
		}
	}

	public static void applyProxy(Channel channel) {
		ProxyInstaller.apply(channel);
	}
}
