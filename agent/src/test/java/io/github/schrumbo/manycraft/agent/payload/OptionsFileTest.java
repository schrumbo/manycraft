package io.github.schrumbo.manycraft.agent.payload;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class OptionsFileTest {
	@TempDir
	Path dir;

	private static Map<String, String> map(String... pairs) {
		Map<String, String> m = new LinkedHashMap<>();
		for (int i = 0; i < pairs.length; i += 2) {
			m.put(pairs[i], pairs[i + 1]);
		}
		return m;
	}

	@Test
	void changedHoldsOnlyWhatTheSaveChanged() {
		var before = OptionsFile.parse(List.of("version:4000", "guiScale:2", "key_key.jump:key.keyboard.space", "resourcePacks:[\"vanilla\"]"));
		var after = OptionsFile.parse(List.of("version:4000", "guiScale:3", "key_key.jump:key.keyboard.space", "resourcePacks:[\"vanilla\",\"file/x.zip\"]", "fov:0.5"));
		assertEquals(map("guiScale", "3", "resourcePacks", "[\"vanilla\",\"file/x.zip\"]"), OptionsFile.changed(before, after));
		assertTrue(OptionsFile.changed(after, after).isEmpty());
	}

	@Test
	void aFreshGameSavingItsDefaultsChangesNothing() {
		assertTrue(OptionsFile.changed(Map.of(), OptionsFile.parse(List.of("guiScale:0", "renderDistance:12", "resourcePacks:[\"vanilla\"]"))).isEmpty());
	}

	@Test
	void valuesTheGameAlreadyHasAreNotAppliedAgain() {
		var current = map("guiScale", "3", "renderDistance", "12");
		assertEquals(map("renderDistance", "16"), OptionsFile.pending(current, map("guiScale", "3", "renderDistance", "16", "bad:key", "1")));
		assertTrue(OptionsFile.pending(current, current).isEmpty());
	}

	@Test
	void aReloadedServerListKeepsTheEntriesTheGameIsEditing() {
		var editing = new String[] {"Farm", "farm.example"};
		var gone = new String[] {"Old", "old.example"};
		var renamed = new String[] {"Farm 2", "farm.example"};
		var added = new String[] {"New", "new.example"};
		var merged = OptionsFile.reconcile(List.of(editing, gone), List.of(renamed, added), e -> e[1], (kept, fresh) -> kept[0] = fresh[0]);
		assertEquals(2, merged.size());
		assertTrue(merged.get(0) == editing);
		assertEquals("Farm 2", editing[0]);
		assertTrue(merged.get(1) == added);
	}

	@Test
	void parseKeepsColonsInValues() {
		assertEquals(map("lastServer", "mc.example:25565"), OptionsFile.parse(List.of("lastServer:mc.example:25565", "no colon", ":nokey")));
	}

	@Test
	void appliedChangesAreOnDiskSoTheNextSaveDoesNotEchoThem() throws Exception {
		Path file = dir.resolve("options.txt");
		Files.writeString(file, "version:4000\nrenderDistance:12\nfullscreen:true\n");
		OptionsFile.apply(file, map("renderDistance", "16", "guiScale", "3"));
		assertEquals("version:4000\nrenderDistance:16\nfullscreen:true\nguiScale:3\n", Files.readString(file));
		var before = OptionsFile.read(file);
		assertTrue(OptionsFile.changed(before, OptionsFile.read(file)).isEmpty());
		assertFalse(Files.exists(dir.resolve("options.txt.manycraft.tmp")));
	}

	@Test
	void applyCreatesAMissingFile() throws Exception {
		Path file = dir.resolve("options.txt");
		OptionsFile.apply(file, map("guiScale", "3"));
		assertEquals("guiScale:3\n", Files.readString(file));
	}

	@Test
	void partialFileCarriesTheVersionFirst() {
		assertEquals("version:4000\nguiScale:3\nkey_key.jump:key.keyboard.j\n", OptionsFile.lines("4000", map("guiScale", "3", "key_key.jump", "key.keyboard.j")));
	}

	@Test
	void brokenKeysAndValuesAreRejected() {
		assertTrue(OptionsFile.valid("guiScale", "3"));
		assertTrue(OptionsFile.valid("resourcePacks", "[\"a:b\"]"));
		assertFalse(OptionsFile.valid("", "3"));
		assertFalse(OptionsFile.valid("a:b", "3"));
		assertFalse(OptionsFile.valid("a\nb", "3"));
		assertFalse(OptionsFile.valid("a", "3\nfov:1"));
		assertFalse(OptionsFile.valid("a", null));
	}

	@Test
	void aHiddenGameStoresExactlyTheBackgroundKeys() {
		assertEquals(Set.of("renderDistance", "simulationDistance", "particles", "renderClouds", "soundCategory_master", "maxFps"), BackgroundController.USER_KEYS);
	}
}
