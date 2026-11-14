package io.github.schrumbo.manycraft;

import io.github.schrumbo.manycraft.mixin.JoinMultiplayerScreenAccessor;
import io.github.schrumbo.manycraft.mixin.OptionsAccessor;
import io.github.schrumbo.manycraft.mixin.ServerListAccessor;
import java.io.File;
import java.io.IOException;
import java.lang.ref.WeakReference;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import net.minecraft.SharedConstants;
import net.minecraft.client.Minecraft;
import net.minecraft.client.Options;
import net.minecraft.client.PreferredGraphicsApi;
import net.minecraft.client.gui.screens.multiplayer.JoinMultiplayerScreen;
import net.minecraft.client.multiplayer.ServerData;
import net.minecraft.client.multiplayer.ServerList;
import net.minecraft.server.packs.repository.PackRepository;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class OptionSync {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	private static final String PART_FILE = "manycraft-options.part";
	private final WhenShown packReload = new WhenShown();
	private boolean applying;
	private WeakReference<JoinMultiplayerScreen> multiplayer = new WeakReference<>(null);

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
		Options options = mc.options;
		File file = options.getFile();
		Map<String, String> changes = OptionsFile.pending(OptionsFile.read(file.toPath()), values);
		if (changes.isEmpty()) {
			return;
		}
		try {
			OptionsFile.apply(file.toPath(), changes);
		} catch (IOException e) {
			LOGGER.warn("cannot write shared options: {}", e.toString());
		}
		List<String> packs = List.copyOf(options.resourcePacks);
		List<String> incompatible = List.copyOf(options.incompatibleResourcePacks);
		if (background == null) {
			load(options, file, changes);
		} else {
			background.applyUser(changes, keys -> load(options, file, keys));
		}
		LOGGER.info("applied shared options {}", changes.keySet());
		if (!packs.equals(options.resourcePacks) || !incompatible.equals(options.incompatibleResourcePacks)) {
			packReload.run(background != null && background.hidden(), () -> reloadPacks(mc));
		}
	}

	void shown() {
		packReload.shown();
	}

	private static void reloadPacks(Minecraft mc) {
		PackRepository repository = mc.getResourcePackRepository();
		repository.reload();
		mc.options.loadSelectedResourcePacks(repository);
		mc.reloadResourcePacks();
	}

	void multiplayerOpened(JoinMultiplayerScreen screen) {
		multiplayer = new WeakReference<>(screen);
	}

	void serversChanged(Minecraft mc) {
		if (!(multiplayer.get() instanceof JoinMultiplayerScreenAccessor screen) || screen.manycraft$servers() == null || screen.manycraft$list() == null) {
			return;
		}
		ServerList fresh = new ServerList(mc);
		fresh.load();
		ServerList servers = screen.manycraft$servers();
		ServerListAccessor current = (ServerListAccessor) servers;
		ServerListAccessor loaded = (ServerListAccessor) fresh;
		List<ServerData> live = current.manycraft$entries();
		List<ServerData> merged = OptionsFile.reconcile(live, loaded.manycraft$entries(), data -> data.ip, ServerData::copyFrom);
		live.clear();
		live.addAll(merged);
		List<ServerData> hidden = current.manycraft$hidden();
		List<ServerData> mergedHidden = OptionsFile.reconcile(hidden, loaded.manycraft$hidden(), data -> data.ip, ServerData::copyFrom);
		hidden.clear();
		hidden.addAll(mergedHidden);
		screen.manycraft$list().updateOnlineServers(servers);
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
