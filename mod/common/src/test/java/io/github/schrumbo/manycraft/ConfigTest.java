package io.github.schrumbo.manycraft;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class ConfigTest {
	private static Config read(Map<String, String> props, Map<String, String> env) {
		return Config.read(props::get, env::get);
	}

	@Test
	void absentIpcMeansNoCompanion() {
		assertNull(read(Map.of("manycraft.token", "leak"), Map.of(Config.ENV_TOKEN, "x")));
		assertNull(read(Map.of("manycraft.ipc", " "), Map.of()));
	}

	@Test
	void tokenComesFromEnvironmentOnly() {
		Config c = read(Map.of("manycraft.ipc", "127.0.0.1:4000", "manycraft.token", "from-prop"), Map.of(Config.ENV_TOKEN, "from-env"));
		assertEquals("from-env", c.token());
		assertEquals("", read(Map.of("manycraft.ipc", "127.0.0.1:4000", "manycraft.token", "from-prop"), Map.of()).token());
	}

	@Test
	void readsAllProperties() {
		Config c = read(Map.of(
			"manycraft.ipc", "127.0.0.1:4000",
			"manycraft.instance", "i-7",
			"manycraft.preview", "/tmp/p.bin",
			"manycraft.hidden", "true",
			"manycraft.proxy", "socks5://u:p@10.0.0.1:1080"
		), Map.of());
		assertEquals("127.0.0.1", c.host());
		assertEquals(4000, c.port());
		assertEquals("i-7", c.instance());
		assertEquals(Path.of("/tmp/p.bin"), c.preview());
		assertTrue(c.hidden());
		assertEquals(new ProxySpec(ProxySpec.Kind.SOCKS5, "10.0.0.1", 1080, "u", "p"), c.proxy());
		assertNull(c.proxyError());
	}

	@Test
	void badProxyIsRememberedNotDropped() {
		Config c = read(Map.of("manycraft.ipc", "127.0.0.1:4000", "manycraft.proxy", "ftp://x:1"), Map.of());
		assertNull(c.proxy());
		assertTrue(c.proxyRequested());
		assertFalse(read(Map.of("manycraft.ipc", "127.0.0.1:4000"), Map.of()).proxyRequested());
	}

	@Test
	void proxyComesFromTheEnvironmentBeforeTheProperty() {
		Map<String, String> props = Map.of("manycraft.ipc", "127.0.0.1:4000", "manycraft.proxy", "socks5://prop:1");
		assertEquals(new ProxySpec(ProxySpec.Kind.SOCKS5, "env", 2, "u", "p"), read(props, Map.of(Config.ENV_PROXY, "socks5://u:p@env:2")).proxy());
		assertEquals(new ProxySpec(ProxySpec.Kind.SOCKS5, "prop", 1, null, null), read(props, Map.of(Config.ENV_PROXY, " ")).proxy());
		assertTrue(read(Map.of("manycraft.ipc", "127.0.0.1:4000"), Map.of(Config.ENV_PROXY, "ftp://x:1")).proxyRequested());
	}

	@Test
	void theManagerIsFoundWhereItLastWroteItsAddress(@TempDir Path dir) throws Exception {
		Path file = dir.resolve("manager.address");
		Config c = read(Map.of("manycraft.ipc", "127.0.0.1:4000", "manycraft.ipcfile", file.toString()), Map.of());
		assertEquals(file, c.ipcFile());
		assertEquals(4000, c.manager().getPort());
		Files.writeString(file, "127.0.0.1:5123\n");
		assertEquals(5123, c.manager().getPort());
		assertEquals("127.0.0.1", c.manager().getHostString());
		Files.writeString(file, "garbage");
		assertEquals(4000, c.manager().getPort());
		assertNull(read(Map.of("manycraft.ipc", "127.0.0.1:4000"), Map.of()).ipcFile());
	}

	@Test
	void malformedIpcThrows() {
		assertThrows(IllegalArgumentException.class, () -> read(Map.of("manycraft.ipc", "nocolon"), Map.of()));
	}

	@Test
	void portOutOfRangeThrows() {
		assertThrows(IllegalArgumentException.class, () -> read(Map.of("manycraft.ipc", "127.0.0.1:70000"), Map.of()));
		assertThrows(IllegalArgumentException.class, () -> read(Map.of("manycraft.ipc", "127.0.0.1:0"), Map.of()));
	}

	@Test
	void proxyParsing() {
		assertEquals(new ProxySpec(ProxySpec.Kind.HTTP, "proxy.example", 8080, null, null), ProxySpec.parse("http://proxy.example:8080"));
		assertEquals(new ProxySpec(ProxySpec.Kind.SOCKS5, "h", 1, "us er", "p@ss"), ProxySpec.parse("socks5://us%20er:p%40ss@h:1"));
		assertThrows(IllegalArgumentException.class, () -> ProxySpec.parse("socks5://hostonly"));
		assertThrows(IllegalArgumentException.class, () -> ProxySpec.parse("socks4://h:1"));
		assertThrows(IllegalArgumentException.class, () -> ProxySpec.parse("::::"));
	}
}
