package io.github.schrumbo.manycraft.agent.payload;

import io.github.schrumbo.manycraft.agent.Proxies;
import io.github.schrumbo.manycraft.agent.ProxySpec;
import io.netty.channel.Channel;

final class ProxyInstaller {
	static final String HANDLER_NAME = "manycraft_proxy";
	private static final int CONNECT_TIMEOUT_MS = 15000;

	private ProxyInstaller() {}

	static void apply(Channel channel) {
		ProxySpec spec = Proxies.spec();
		if (spec == null) {
			return;
		}
		channel.pipeline().addFirst(HANDLER_NAME, new ProxyHandler(spec, Proxies.address(spec), CONNECT_TIMEOUT_MS));
	}
}
