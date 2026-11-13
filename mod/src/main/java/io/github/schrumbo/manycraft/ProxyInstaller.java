package io.github.schrumbo.manycraft;

import io.netty.channel.Channel;
import io.netty.handler.proxy.HttpProxyHandler;
import io.netty.handler.proxy.ProxyHandler;
import io.netty.handler.proxy.Socks5ProxyHandler;
import java.net.InetSocketAddress;

final class ProxyInstaller {
	static final String HANDLER_NAME = "manycraft_proxy";
	private static final int CONNECT_TIMEOUT_MS = 15000;

	private ProxyInstaller() {}

	static void apply(Channel channel) {
		if (System.getProperty("manycraft.ipc") == null) {
			return;
		}
		String raw = System.getProperty("manycraft.proxy");
		if (raw == null || raw.isBlank()) {
			return;
		}
		ProxySpec spec;
		try {
			spec = ProxySpec.parse(raw);
		} catch (RuntimeException e) {
			throw new IllegalStateException("proxy configured but invalid, refusing direct connection: " + e.getMessage());
		}
		InetSocketAddress address = new InetSocketAddress(spec.host(), spec.port());
		if (address.isUnresolved()) {
			throw new IllegalStateException("proxy host unresolved, refusing direct connection: " + spec.host());
		}
		ProxyHandler handler = switch (spec.kind()) {
			case SOCKS5 -> spec.hasCredentials()
				? new Socks5ProxyHandler(address, spec.user(), spec.password())
				: new Socks5ProxyHandler(address);
			case HTTP -> spec.hasCredentials()
				? new HttpProxyHandler(address, spec.user(), spec.password())
				: new HttpProxyHandler(address);
		};
		handler.setConnectTimeoutMillis(CONNECT_TIMEOUT_MS);
		channel.pipeline().addFirst(HANDLER_NAME, handler);
	}
}
