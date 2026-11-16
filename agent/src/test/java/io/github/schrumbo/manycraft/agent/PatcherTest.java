package io.github.schrumbo.manycraft.agent;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.IOException;
import java.io.InputStream;
import java.lang.classfile.ClassFile;
import java.lang.classfile.ClassModel;
import java.lang.classfile.FieldModel;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

class PatcherTest {
	private static byte[] bytes(String name) throws IOException {
		try (InputStream in = PatcherTest.class.getClassLoader().getResourceAsStream(name + ".class")) {
			assertNotNull(in, name);
			return in.readAllBytes();
		}
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
			Patcher.files(loader).verify(out).forEach(error -> problems.add(e.getKey() + ": " + error.getMessage()));
		}
		assertEquals(List.of(), problems);
		Patcher.table(true).values().stream().flatMap(List::stream).filter(h -> !h.alternative()).forEach(h -> assertTrue(Patches.has(h.feature()), h.feature()));
		assertTrue(Patches.has(Patches.CORE));
		assertTrue(Patches.has("close"));
		assertTrue(Patches.has("hud"));
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
}
