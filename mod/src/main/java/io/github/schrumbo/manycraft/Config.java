package io.github.schrumbo.manycraft;

import java.nio.file.Path;
import java.util.function.UnaryOperator;

record Config(String host, int port, String token, String instance, Path preview, ProxySpec proxy, String proxyError, boolean hidden) {
	static final String ENV_TOKEN = "MANYCRAFT_TOKEN";

	static Config read(UnaryOperator<String> props, UnaryOperator<String> env) {
		String ipc = props.apply("manycraft.ipc");
		if (ipc == null || ipc.isBlank()) {
			return null;
		}
		int colon = ipc.lastIndexOf(':');
		if (colon <= 0) {
			throw new IllegalArgumentException("manycraft.ipc must be host:port");
		}
		String host = ipc.substring(0, colon);
		int port = Integer.parseInt(ipc.substring(colon + 1).trim());
		if (port < 1 || port > 65535) {
			throw new IllegalArgumentException("manycraft.ipc port out of range");
		}
		String token = env.apply(ENV_TOKEN);
		String previewPath = props.apply("manycraft.preview");
		ProxySpec proxy = null;
		String proxyError = null;
		String rawProxy = props.apply("manycraft.proxy");
		if (rawProxy != null && !rawProxy.isBlank()) {
			try {
				proxy = ProxySpec.parse(rawProxy);
			} catch (IllegalArgumentException e) {
				proxyError = e.getMessage();
			}
		}
		return new Config(
			host,
			port,
			token == null ? "" : token,
			props.apply("manycraft.instance") == null ? "" : props.apply("manycraft.instance"),
			previewPath == null || previewPath.isBlank() ? null : Path.of(previewPath),
			proxy,
			proxyError,
			Boolean.parseBoolean(props.apply("manycraft.hidden"))
		);
	}

	boolean proxyRequested() {
		return proxy != null || proxyError != null;
	}
}
