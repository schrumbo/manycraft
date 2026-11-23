package io.github.schrumbo.manycraft.agent;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.IOException;
import java.io.InputStream;
import java.net.URI;
import java.net.URL;
import java.nio.file.Files;
import java.nio.file.Path;
import java.lang.classfile.ClassFile;
import java.lang.classfile.ClassModel;
import java.lang.classfile.FieldModel;
import java.lang.classfile.MethodModel;
import java.lang.classfile.instruction.InvokeInstruction;
import java.lang.constant.ClassDesc;
import java.lang.constant.ConstantDescs;
import java.lang.constant.MethodTypeDesc;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.jar.JarEntry;
import java.util.jar.JarOutputStream;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class PatcherTest {
	private static byte[] bytes(String name) throws IOException {
		try (InputStream in = PatcherTest.class.getClassLoader().getResourceAsStream(name + ".class")) {
			assertNotNull(in, name);
			return in.readAllBytes();
		}
	}

	@Test
	void headlessFrameHookVerifiesIn26_3() throws IOException {
		ClassLoader loader = PatcherTest.class.getClassLoader();
		String name = "net/minecraft/client/Minecraft";
		byte[] out = Patcher.patch(loader, name, bytes(name), Patcher.table(true, true).get(name));
		assertNotNull(out);
		assertEquals(List.of(), Patcher.verify(Patcher.files(loader), out).stream().map(Throwable::getMessage).toList());
	}

	@Test
	void everyHookFindsItsTargetIn26_3() throws IOException {
		ClassLoader loader = PatcherTest.class.getClassLoader();
		List<String> problems = new ArrayList<>();
		for (Map.Entry<String, List<Patcher.Hook>> e : Patcher.table(true).entrySet()) {
			if (e.getValue().stream().allMatch(Patcher.Hook::alternative)) {
				continue;
			}
			byte[] out = Patcher.patch(loader, e.getKey(), bytes(e.getKey()), e.getValue());
			assertNotNull(out, e.getKey());
			Patcher.verify(Patcher.files(loader), out).forEach(error -> problems.add(e.getKey() + ": " + error.getMessage()));
		}
		assertEquals(List.of(), problems);
		Patcher.table(true).values().stream().flatMap(List::stream).filter(h -> !h.alternative()).forEach(h -> assertTrue(Patches.has(h.feature()), h.feature()));
		assertTrue(Patches.has(Patches.CORE));
		assertTrue(Patches.has("close"));
		assertTrue(Patches.has("quit"));
		assertTrue(Patches.has("hud"));
	}

	@Test
	void extractAndRenderShareOneAnswerPerFrame() throws IOException {
		String name = "net/minecraft/client/renderer/GameRenderer";
		byte[] out = Patcher.patch(PatcherTest.class.getClassLoader(), name, bytes(name), Patcher.table(false).get(name));
		assertNotNull(out);
		assertEquals(List.of("beforeExtract"), hookCalls(out, "extract"));
		assertEquals("drawing", hookCalls(out, "render").getFirst());
	}

	private static List<String> hookCalls(byte[] patched, String method) {
		MethodModel model = ClassFile.of().parse(patched).methods().stream().filter(m -> m.methodName().equalsString(method)).findFirst().orElseThrow();
		return model.code().orElseThrow().elementStream()
			.filter(e -> e instanceof InvokeInstruction i && i.owner().asInternalName().equals("io/github/schrumbo/manycraft/agent/Hooks"))
			.map(e -> ((InvokeInstruction) e).name().stringValue()).toList();
	}

	@Test
	void anAlternativeThatMissesNeitherCountsNorTouchesTheClass() throws IOException {
		String name = "net/minecraft/client/gui/Gui";
		assertNull(Patcher.patch(PatcherTest.class.getClassLoader(), name, bytes(name), Patcher.table(false).get(name)));
	}

	@Test
	void optionsFileBecomesWritableForSharedOptions() throws IOException {
		String name = "net/minecraft/client/Options";
		byte[] out = Patcher.patch(PatcherTest.class.getClassLoader(), name, bytes(name), Patcher.table(false).get(name));
		ClassModel model = ClassFile.of().parse(out);
		FieldModel field = model.fields().stream().filter(f -> f.fieldName().equalsString("optionsFile")).findFirst().orElseThrow();
		assertEquals(0, field.flags().flagsMask() & ClassFile.ACC_FINAL);
	}

	@Test
	void aClassWithoutTheTargetsStaysUntouched() throws IOException {
		String name = "net/minecraft/client/Options";
		List<Patcher.Hook> wrong = List.of(new Patcher.Hook("test-missing", "noSuchMethod", "()V", hit -> (b, e) -> b.with(e)));
		assertNull(Patcher.patch(PatcherTest.class.getClassLoader(), name, bytes(name), wrong));
		assertEquals(false, Patches.applied().get("test-missing"));
	}

	@Test
	void proxiedNamesStillFollowRedirectsAndTheBlocklist() throws IOException {
		String name = "net/minecraft/client/multiplayer/resolver/ServerNameResolver";
		byte[] out = Patcher.patch(PatcherTest.class.getClassLoader(), name, bytes(name), Patcher.table(true).get(name));
		MethodModel resolve = ClassFile.of().parse(out).methods().stream().filter(m -> m.methodName().equalsString("resolveAddress")).findFirst().orElseThrow();
		List<String> calls = resolve.code().orElseThrow().elementStream().filter(e -> e instanceof InvokeInstruction).map(e -> ((InvokeInstruction) e).name().stringValue()).toList();
		assertEquals(2, calls.stream().filter("remoteName"::equals).count());
		assertTrue(calls.contains("lookupRedirect"));
		assertTrue(calls.contains("isAllowed"));
	}

	@Test
	void aPatchThatDoesNotVerifyLeavesTheClassAlone() throws IOException {
		String name = "net/minecraft/client/Options";
		List<Patcher.Hook> broken = List.of(new Patcher.Hook("test-broken", "save", "()V", Patcher.head(b -> b.aconst_null().areturn())));
		assertNull(Patcher.patch(PatcherTest.class.getClassLoader(), name, bytes(name), broken));
		assertEquals(false, Patches.applied().get("test-broken"));
	}

	@Test
	void theHudHookFollowsADroppedDeltaParameter() {
		String name = "net/minecraft/client/gui/Hud";
		ClassDesc graphics = ClassDesc.ofInternalName("net/minecraft/client/gui/GuiGraphicsExtractor");
		byte[] newer = ClassFile.of().build(ClassDesc.ofInternalName(name), cb -> cb.withMethodBody("extractRenderState", MethodTypeDesc.of(ConstantDescs.CD_void, graphics), ClassFile.ACC_PUBLIC, b -> b.return_()));
		byte[] out = Patcher.patch(PatcherTest.class.getClassLoader(), name, newer, Patcher.table(false).get(name));
		assertNotNull(out);
		MethodModel extract = ClassFile.of().parse(out).methods().stream().filter(m -> m.methodName().equalsString("extractRenderState")).findFirst().orElseThrow();
		assertTrue(extract.code().orElseThrow().elementStream().anyMatch(e -> e instanceof InvokeInstruction i && i.name().equalsString("hud")));
	}

	@Test
	void classesBehindABrokenJarSignatureStillResolve(@TempDir Path dir) throws IOException {
		byte[] bytes = bytes("io/github/schrumbo/manycraft/agent/Patches");
		Path jar = dir.resolve("client.jar");
		try (JarOutputStream out = new JarOutputStream(Files.newOutputStream(jar))) {
			out.putNextEntry(new JarEntry("io/github/schrumbo/manycraft/agent/Patches.class"));
			out.write(bytes);
		}
		ClassLoader signed = new ClassLoader(null) {
			@Override
			public InputStream getResourceAsStream(String name) {
				return new InputStream() {
					@Override
					public int read() {
						throw new SecurityException("SHA-384 digest error for " + name);
					}
				};
			}

			@Override
			public URL getResource(String name) {
				try {
					return URI.create("jar:" + jar.toUri() + "!/" + name).toURL();
				} catch (IOException e) {
					throw new IllegalStateException(e);
				}
			}
		};
		try (InputStream in = Patcher.resource(signed, ClassDesc.of("io.github.schrumbo.manycraft.agent.Patches"))) {
			assertNotNull(in);
			assertEquals(bytes.length, in.readAllBytes().length);
		}
	}
}
