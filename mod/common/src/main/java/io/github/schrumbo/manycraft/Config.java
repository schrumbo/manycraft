package io.github.schrumbo.manycraft;

import java.io.IOException;
import java.net.InetSocketAddress;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.function.UnaryOperator;

record Config(String host, int port, String token, String instance, Path preview, ProxySpec proxy, String proxyError, boolean hidden, Path ipcFile) {
	static final String ENV_TOKEN = "MANYCRAFT_TOKEN";
	static final String ENV_PROXY = "MANYCRAFT_PROXY";

	static String rawProxy(UnaryOperator<String> props, UnaryOperator<String> env) {
		String fromEnv = env.apply(ENV_PROXY);
		if (fromEnv != null && !fromEnv.isBlank()) {
			return fromEnv;
		}
		String fromProperty = props.apply("manycraft.proxy");
		return fromProperty == null || fromProperty.isBlank() ? null : fromProperty;
	}

	static InetSocketAddress address(String ipc) {
		int colon = ipc.lastIndexOf(':');
		if (colon <= 0) {
			throw new IllegalArgumentException("manycraft.ipc must be host:port");
		}
		String host = ipc.substring(0, colon);
		int port = Integer.parseInt(ipc.substring(colon + 1).trim());
		if (port < 1 || port > 65535) {
			throw new IllegalArgumentException("manycraft.ipc port out of range");
		}
		return InetSocketAddress.createUnresolved(host, port);
	}

	static Config read(UnaryOperator<String> props, UnaryOperator<String> env) {
		String ipc = props.apply("manycraft.ipc");
		if (ipc == null || ipc.isBlank()) {
			return null;
		}
		InetSocketAddress at = address(ipc);
		String token = env.apply(ENV_TOKEN);
		String previewPath = props.apply("manycraft.preview");
		String ipcFile = props.apply("manycraft.ipcfile");
		ProxySpec proxy = null;
		String proxyError = null;
		String rawProxy = rawProxy(props, env);
		if (rawProxy != null) {
			try {
				proxy = ProxySpec.parse(rawProxy);
			} catch (IllegalArgumentException e) {
				proxyError = e.getMessage();
			}
		}
		return new Config(
			at.getHostString(),
			at.getPort(),
			token == null ? "" : token,
			props.apply("manycraft.instance") == null ? "" : props.apply("manycraft.instance"),
			previewPath == null || previewPath.isBlank() ? null : Path.of(previewPath),
			proxy,
			proxyError,
			Boolean.parseBoolean(props.apply("manycraft.hidden")),
			ipcFile == null || ipcFile.isBlank() ? null : Path.of(ipcFile)
		);
	}

	InetSocketAddress manager() {
		if (ipcFile != null) {
			try {
				InetSocketAddress written = address(Files.readString(ipcFile).trim());
				return new InetSocketAddress(written.getHostString(), written.getPort());
			} catch (IOException | RuntimeException e) {
				return new InetSocketAddress(host, port);
			}
		}
		return new InetSocketAddress(host, port);
	}

	boolean proxyRequested() {
		return proxy != null || proxyError != null;
	}
}
