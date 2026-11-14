package io.github.schrumbo.manycraft;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.Proxy;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

class ProxyInstallerTest {
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
		InetSocketAddress placeholder = ProxyInstaller.remoteName("mc.example.org", 25565).orElseThrow();
		assertEquals("mc.example.org", placeholder.getHostString());
		assertEquals(25565, placeholder.getPort());
		InetSocketAddress forProxy = (InetSocketAddress) ProxyInstaller.forProxy(placeholder);
		assertTrue(forProxy.isUnresolved());
		assertEquals("mc.example.org", forProxy.getHostString());
		assertEquals(25565, forProxy.getPort());
	}

	@Test
	void literalAddressesNeedNoRemoteLookup() throws Exception {
		assertTrue(ProxyInstaller.remoteName("10.1.2.3", 25565).isEmpty());
		assertTrue(ProxyInstaller.remoteName("::1", 25565).isEmpty());
		InetSocketAddress literal = new InetSocketAddress(InetAddress.getByName("10.1.2.3"), 1);
		assertEquals(literal, ProxyInstaller.forProxy(literal));
	}

	@Test
	void withoutAProxyNothingChanges() {
		System.clearProperty("manycraft.proxy");
		assertFalse(ProxyInstaller.requested());
		assertTrue(ProxyInstaller.remoteName("mc.example.org", 25565).isEmpty());
		assertEquals(null, ProxyInstaller.javaProxy());
	}

	@Test
	void withoutTheManagerNothingChanges() {
		System.clearProperty("manycraft.ipc");
		assertFalse(ProxyInstaller.requested());
	}

	@Test
	void mojangServicesGoThroughTheProxy() {
		Proxy proxy = ProxyInstaller.javaProxy();
		assertEquals(Proxy.Type.SOCKS, proxy.type());
		assertEquals(1080, ((InetSocketAddress) proxy.address()).getPort());
	}

	@Test
	void anUnusableProxyBlocksInsteadOfGoingDirect() {
		System.setProperty("manycraft.proxy", "ftp://x:1");
		Proxy proxy = ProxyInstaller.javaProxy();
		InetSocketAddress dead = assertInstanceOf(InetSocketAddress.class, proxy.address());
		assertTrue(dead.getAddress().isLoopbackAddress());
		assertEquals(9, dead.getPort());
	}
}
