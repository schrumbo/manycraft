package io.github.schrumbo.manycraft.agent;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.Proxy;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

class ProxiesTest {
	@BeforeEach
	void configure() {
		System.setProperty("manycraft.ipc", "127.0.0.1:4000");
		System.setProperty("manycraft.proxy", "socks5://u:p@127.0.0.1:1080");
	}

	@AfterEach
	void reset() {
		System.clearProperty("manycraft.ipc");
		System.clearProperty("manycraft.proxy");
	}

	@Test
	void namesStayUnresolvedForTheProxy() throws Exception {
		InetSocketAddress placeholder = Proxies.remoteName("mc.example.org", 25565);
		assertEquals("mc.example.org", placeholder.getHostString());
		assertEquals(25565, placeholder.getPort());
		InetSocketAddress forProxy = (InetSocketAddress) Proxies.forProxy(placeholder);
		assertTrue(forProxy.isUnresolved());
		assertEquals("mc.example.org", forProxy.getHostString());
		assertEquals(25565, forProxy.getPort());
	}

	@Test
	void literalAddressesNeedNoRemoteLookup() throws Exception {
		assertNull(Proxies.remoteName("10.1.2.3", 25565));
		assertNull(Proxies.remoteName("::1", 25565));
		InetSocketAddress literal = new InetSocketAddress(InetAddress.getByName("10.1.2.3"), 1);
		assertEquals(literal, Proxies.forProxy(literal));
	}

	@Test
	void withoutAProxyNothingChanges() {
		System.clearProperty("manycraft.proxy");
		assertFalse(Proxies.requested());
		assertNull(Proxies.remoteName("mc.example.org", 25565));
		assertEquals(null, Proxies.javaProxy());
	}

	@Test
	void withoutTheManagerNothingChanges() {
		System.clearProperty("manycraft.ipc");
		assertFalse(Proxies.requested());
	}

	@Test
	void mojangServicesGoThroughTheProxy() {
		Proxy proxy = Proxies.javaProxy();
		assertEquals(Proxy.Type.SOCKS, proxy.type());
		assertEquals(1080, ((InetSocketAddress) proxy.address()).getPort());
	}

	@Test
	void anUnusableProxyBlocksInsteadOfGoingDirect() {
		System.setProperty("manycraft.proxy", "ftp://x:1");
		Proxy proxy = Proxies.javaProxy();
		InetSocketAddress dead = assertInstanceOf(InetSocketAddress.class, proxy.address());
		assertTrue(dead.getAddress().isLoopbackAddress());
		assertEquals(9, dead.getPort());
	}
}
