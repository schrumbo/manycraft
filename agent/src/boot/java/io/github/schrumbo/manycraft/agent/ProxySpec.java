package io.github.schrumbo.manycraft.agent;

import java.net.URI;
import java.net.URISyntaxException;
import java.net.URLDecoder;
import java.nio.charset.StandardCharsets;
import java.util.Locale;

/**
 * A proxy URL from the manager: {@code socks5://} or {@code http://}, with optional credentials.
 */
public record ProxySpec(Kind kind, String host, int port, String user, String password) {
	public enum Kind { SOCKS5, HTTP }

	public static ProxySpec parse(String raw) {
		URI uri;
		try {
			uri = new URI(raw.trim());
		} catch (URISyntaxException e) {
			throw new IllegalArgumentException("invalid proxy URL");
		}
		String scheme = uri.getScheme() == null ? "" : uri.getScheme().toLowerCase(Locale.ROOT);
		Kind kind = switch (scheme) {
			case "socks5" -> Kind.SOCKS5;
			case "http" -> Kind.HTTP;
			default -> throw new IllegalArgumentException("unsupported proxy scheme: " + scheme);
		};
		String host = uri.getHost();
		int port = uri.getPort();
		if (host == null || host.isEmpty() || port < 1 || port > 65535) {
			throw new IllegalArgumentException("proxy needs host and port");
		}
		String user = null;
		String password = null;
		String info = uri.getRawUserInfo();
		if (info != null && !info.isEmpty()) {
			int colon = info.indexOf(':');
			user = decode(colon < 0 ? info : info.substring(0, colon));
			password = colon < 0 ? "" : decode(info.substring(colon + 1));
		}
		return new ProxySpec(kind, host, port, user, password);
	}

	public boolean hasCredentials() {
		return user != null;
	}

	@Override
	public String toString() {
		return kind + "://" + host + ":" + port;
	}

	private static String decode(String value) {
		return URLDecoder.decode(value.replace("+", "%2B"), StandardCharsets.UTF_8);
	}
}
