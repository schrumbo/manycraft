package io.github.schrumbo.manycraft.agent;

import java.io.IOException;
import java.net.Authenticator;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.PasswordAuthentication;
import java.net.Proxy;
import java.net.ProxySelector;
import java.net.SocketAddress;
import java.net.URI;
import java.net.UnknownHostException;
import java.util.List;
import java.util.function.UnaryOperator;
import java.util.regex.Pattern;

/**
 * The account's proxy as the manager passed it, and the JVM-wide settings that send Mojang's services through it.
 */
public final class Proxies {
	public static final String ENV_PROXY = "MANYCRAFT_PROXY";
	private static final byte[] PLACEHOLDER = {0, 0, 0, 1};
	private static final Pattern IPV4 = Pattern.compile("\\d{1,3}(\\.\\d{1,3}){3}");
	private static volatile boolean defaultsInstalled;

	private Proxies() {}

	public static String raw(UnaryOperator<String> props, UnaryOperator<String> env) {
		if (props.apply("manycraft.ipc") == null) {
			return null;
		}
		String fromEnv = env.apply(ENV_PROXY);
		if (fromEnv != null && !fromEnv.isBlank()) {
			return fromEnv;
		}
		String fromProperty = props.apply("manycraft.proxy");
		return fromProperty == null || fromProperty.isBlank() ? null : fromProperty;
	}

	public static String raw() {
		return raw(System::getProperty, System::getenv);
	}

	public static boolean requested() {
		return raw() != null;
	}

	/**
	 * The parsed proxy, or null without one. Throws when one is configured but unusable, so callers refuse to go direct.
	 */
	public static ProxySpec spec() {
		String raw = raw();
		if (raw == null) {
			return null;
		}
		try {
			return ProxySpec.parse(raw);
		} catch (RuntimeException e) {
			throw new IllegalStateException("proxy configured but invalid, refusing direct connection: " + e.getMessage());
		}
	}

	public static InetSocketAddress address(ProxySpec spec) {
		InetSocketAddress address = new InetSocketAddress(spec.host(), spec.port());
		if (address.isUnresolved()) {
			throw new IllegalStateException("proxy host unresolved, refusing direct connection: " + spec.host());
		}
		return address;
	}

	public static Proxy javaProxy() {
		if (!requested()) {
			return null;
		}
		try {
			ProxySpec spec = spec();
			return new Proxy(spec.kind() == ProxySpec.Kind.HTTP ? Proxy.Type.HTTP : Proxy.Type.SOCKS, address(spec));
		} catch (RuntimeException e) {
			Agent.log("proxy unusable, blocking Mojang services: " + e.getMessage());
			return new Proxy(Proxy.Type.SOCKS, new InetSocketAddress(InetAddress.getLoopbackAddress(), 9));
		}
	}

	static void installDefaults() {
		Proxy proxy = javaProxy();
		if (proxy == null || defaultsInstalled) {
			return;
		}
		defaultsInstalled = true;
		ProxySpec spec;
		try {
			spec = spec();
		} catch (RuntimeException e) {
			spec = null;
		}
		System.setProperty("jdk.http.auth.tunneling.disabledSchemes", "");
		ProxySelector.setDefault(new ProxySelector() {
			@Override
			public List<Proxy> select(URI uri) {
				String host = uri == null ? null : uri.getHost();
				return host != null && isLoopback(host) ? List.of(Proxy.NO_PROXY) : List.of(proxy);
			}

			@Override
			public void connectFailed(URI uri, SocketAddress address, IOException failure) {}
		});
		if (spec != null && spec.hasCredentials()) {
			ProxySpec signIn = spec;
			Authenticator.setDefault(new Authenticator() {
				@Override
				protected PasswordAuthentication getPasswordAuthentication() {
					if (getRequestingPort() != signIn.port() || !asksFor(signIn)) {
						return null;
					}
					return new PasswordAuthentication(signIn.user(), signIn.password().toCharArray());
				}

				private boolean asksFor(ProxySpec spec) {
					String host = getRequestingHost();
					InetAddress site = getRequestingSite();
					return spec.host().equalsIgnoreCase(host)
						|| host != null && host.equals(site == null ? null : site.getHostAddress())
						|| site != null && spec.host().equals(site.getHostAddress());
				}
			});
		}
	}

	/**
	 * A stand-in address for {@code host} that keeps the name, so the proxy resolves it; null for literal addresses or
	 * without a proxy.
	 */
	public static InetSocketAddress remoteName(String host, int port) {
		if (!requested() || host == null || isLiteral(host)) {
			return null;
		}
		try {
			return new InetSocketAddress(InetAddress.getByAddress(host, PLACEHOLDER), port);
		} catch (UnknownHostException e) {
			return null;
		}
	}

	/**
	 * What the proxy should connect to: the name of a stand-in address, unresolved, otherwise {@code remote} itself.
	 */
	public static SocketAddress forProxy(SocketAddress remote) {
		if (remote instanceof InetSocketAddress target && !target.isUnresolved() && !isLiteral(target.getHostString())) {
			return InetSocketAddress.createUnresolved(target.getHostString(), target.getPort());
		}
		return remote;
	}

	static boolean isLiteral(String host) {
		return host.indexOf(':') >= 0 || IPV4.matcher(host).matches();
	}

	private static boolean isLoopback(String host) {
		return host.equalsIgnoreCase("localhost") || host.startsWith("127.") || host.equals("::1") || host.equals("[::1]");
	}
}
