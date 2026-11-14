package io.github.schrumbo.manycraft;

import io.netty.channel.Channel;
import io.netty.channel.ChannelHandlerContext;
import io.netty.channel.ChannelOutboundHandlerAdapter;
import io.netty.channel.ChannelPromise;
import io.netty.handler.proxy.HttpProxyHandler;
import io.netty.handler.proxy.ProxyHandler;
import io.netty.handler.proxy.Socks5ProxyHandler;
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
import java.util.Optional;
import java.util.regex.Pattern;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class ProxyInstaller {
	static final String HANDLER_NAME = "manycraft_proxy";
	static final String DNS_HANDLER_NAME = "manycraft_proxy_dns";
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	private static final int CONNECT_TIMEOUT_MS = 15000;
	private static final byte[] PLACEHOLDER = {0, 0, 0, 1};
	private static final Pattern IPV4 = Pattern.compile("\\d{1,3}(\\.\\d{1,3}){3}");
	private static volatile boolean defaultsInstalled;

	private ProxyInstaller() {}

	static String raw() {
		if (System.getProperty("manycraft.ipc") == null) {
			return null;
		}
		return Config.rawProxy(System::getProperty, System::getenv);
	}

	static boolean requested() {
		return raw() != null;
	}

	private static ProxySpec spec() {
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

	private static InetSocketAddress address(ProxySpec spec) {
		InetSocketAddress address = new InetSocketAddress(spec.host(), spec.port());
		if (address.isUnresolved()) {
			throw new IllegalStateException("proxy host unresolved, refusing direct connection: " + spec.host());
		}
		return address;
	}

	static void apply(Channel channel) {
		ProxySpec spec = spec();
		if (spec == null) {
			return;
		}
		InetSocketAddress address = address(spec);
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
		channel.pipeline().addAfter(HANDLER_NAME, DNS_HANDLER_NAME, new RemoteDns());
	}

	static Proxy javaProxy() {
		if (!requested()) {
			return null;
		}
		try {
			ProxySpec spec = spec();
			return new Proxy(spec.kind() == ProxySpec.Kind.HTTP ? Proxy.Type.HTTP : Proxy.Type.SOCKS, address(spec));
		} catch (RuntimeException e) {
			LOGGER.warn("proxy unusable, blocking Mojang services: {}", e.getMessage());
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
			public void connectFailed(URI uri, SocketAddress address, java.io.IOException failure) {
				LOGGER.debug("proxy connect failed: {}", failure.toString());
			}
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

	static Optional<InetSocketAddress> remoteName(String host, int port) {
		if (!requested() || host == null || isLiteral(host)) {
			return Optional.empty();
		}
		try {
			return Optional.of(new InetSocketAddress(InetAddress.getByAddress(host, PLACEHOLDER), port));
		} catch (UnknownHostException e) {
			return Optional.empty();
		}
	}

	static SocketAddress forProxy(SocketAddress remote) {
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

	private static final class RemoteDns extends ChannelOutboundHandlerAdapter {
		@Override
		public void connect(ChannelHandlerContext ctx, SocketAddress remote, SocketAddress local, ChannelPromise promise) throws Exception {
			ctx.connect(forProxy(remote), local, promise);
		}
	}
}
