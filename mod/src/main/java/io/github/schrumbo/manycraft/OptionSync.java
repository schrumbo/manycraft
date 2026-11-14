package io.github.schrumbo.manycraft;

import io.github.schrumbo.manycraft.mixin.JoinMultiplayerScreenAccessor;
import io.github.schrumbo.manycraft.mixin.OptionsAccessor;
import java.io.File;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import net.minecraft.SharedConstants;
import net.minecraft.client.Minecraft;
import net.minecraft.client.Options;
import net.minecraft.client.PreferredGraphicsApi;
import net.minecraft.client.gui.screens.multiplayer.JoinMultiplayerScreen;
import net.minecraft.server.packs.repository.PackRepository;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class OptionSync {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	private static final String PART_FILE = "manycraft-options.part";
	private boolean applying;

	Map<String, String> save(Options options, Runnable save) {
		if (applying) {
			save.run();
			return Map.of();
		}
		Path path = options.getFile().toPath();
		Map<String, String> before = OptionsFile.read(path);
		save.run();
		return OptionsFile.changed(before, OptionsFile.read(path));
	}

	void apply(Minecraft mc, Map<String, String> values, BackgroundController background) {
		Map<String, String> changes = new LinkedHashMap<>();
		values.forEach((key, value) -> {
			if (OptionsFile.valid(key, value)) {
				changes.put(key, value);
			}
		});
		if (changes.isEmpty()) {
			return;
		}
		Options options = mc.options;
		File file = options.getFile();
		try {
			OptionsFile.apply(file.toPath(), changes);
		} catch (IOException e) {
			LOGGER.warn("cannot write shared options: {}", e.toString());
		}
		List<String> packs = List.copyOf(options.resourcePacks);
		List<String> incompatible = List.copyOf(options.incompatibleResourcePacks);
		Runnable load = () -> load(options, file, changes);
		if (background == null) {
			load.run();
		} else {
			background.applyUser(changes.keySet(), load);
		}
		LOGGER.info("applied shared options {}", changes.keySet());
		if (!packs.equals(options.resourcePacks) || !incompatible.equals(options.incompatibleResourcePacks)) {
			PackRepository repository = mc.getResourcePackRepository();
			repository.reload();
			options.loadSelectedResourcePacks(repository);
			mc.reloadResourcePacks();
		}
	}

	void serversChanged(Minecraft mc) {
		if (mc.gui.screen() instanceof JoinMultiplayerScreen screen) {
			((JoinMultiplayerScreenAccessor) screen).manycraft$refreshServerList();
		}
	}

	private void load(Options options, File file, Map<String, String> changes) {
		File part = new File(file.getParentFile(), PART_FILE);
		OptionsAccessor accessor = (OptionsAccessor) options;
		PreferredGraphicsApi startup = accessor.manycraft$getGraphicsBackendFromStartup();
		applying = true;
		try {
			String version = Integer.toString(SharedConstants.getCurrentVersion().dataVersion().version());
			Files.writeString(part.toPath(), OptionsFile.lines(version, changes), StandardCharsets.UTF_8);
			accessor.manycraft$setOptionsFile(part);
			options.load();
		} catch (IOException e) {
			LOGGER.warn("cannot apply shared options: {}", e.toString());
		} finally {
			accessor.manycraft$setOptionsFile(file);
			accessor.manycraft$setGraphicsBackendFromStartup(startup);
			applying = false;
			try {
				Files.deleteIfExists(part.toPath());
			} catch (IOException ignored) {
			}
		}
	}
}
