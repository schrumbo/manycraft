package io.github.schrumbo.manycraft;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import net.minecraft.client.CloudStatus;
import net.minecraft.server.level.ParticleStatus;
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
		assertEquals(map("guiScale", "3", "resourcePacks", "[\"vanilla\",\"file/x.zip\"]", "fov", "0.5"), OptionsFile.changed(before, after));
		assertTrue(OptionsFile.changed(after, after).isEmpty());
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
	void hiddenGameTakesTheSharedValuesAsThePlayersOwn() {
		var saved = new BackgroundController.Values(12, 10, ParticleStatus.ALL, CloudStatus.FANCY, 0.7, 120);
		var live = new BackgroundController.Values(16, 8, ParticleStatus.DECREASED, CloudStatus.FAST, 0.2, 60);
		assertEquals(new BackgroundController.Values(16, 10, ParticleStatus.ALL, CloudStatus.FANCY, 0.7, 60), saved.adopt(Set.of("renderDistance", "maxFps", "guiScale"), live));
		assertEquals(live, saved.adopt(Set.of("renderDistance", "simulationDistance", "particles", "renderClouds", "soundCategory_master", "maxFps"), live));
		assertEquals(saved, saved.adopt(Set.of(), live));
	}
}
