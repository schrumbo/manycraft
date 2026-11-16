package io.github.schrumbo.manycraft.agent.payload;

import java.io.File;
import java.io.IOException;
import java.lang.ref.WeakReference;
import java.lang.reflect.Field;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import net.minecraft.SharedConstants;
import net.minecraft.client.Minecraft;
import net.minecraft.client.Options;
import net.minecraft.client.gui.screens.multiplayer.JoinMultiplayerScreen;
import net.minecraft.client.gui.screens.multiplayer.ServerSelectionList;
import net.minecraft.client.multiplayer.ServerData;
import net.minecraft.client.multiplayer.ServerList;
import net.minecraft.server.packs.repository.PackRepository;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class OptionSync {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	private static final String PART_FILE = "manycraft-options.part";
	private static final Field SERVERS = Access.field(JoinMultiplayerScreen.class, "servers");
	private static final Field SERVER_SELECTION = Access.field(JoinMultiplayerScreen.class, "serverSelectionList");
	private static final Field SERVER_LIST = Access.field(ServerList.class, "serverList");
	private static final Field HIDDEN_SERVER_LIST = Access.field(ServerList.class, "hiddenServerList");
	private static final Field OPTIONS_FILE = Access.field(Options.class, "optionsFile");
	private static final Field STARTUP_BACKEND = Access.field(Options.class, "preferredGraphicsBackendFromStartup");
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
		JoinMultiplayerScreen screen = multiplayer.get();
		ServerList servers = screen == null ? null : Access.get(SERVERS, screen);
		ServerSelectionList list = screen == null ? null : Access.get(SERVER_SELECTION, screen);
		if (servers == null || list == null) {
			return;
		}
		ServerList fresh = new ServerList(mc);
		fresh.load();
		List<ServerData> live = Access.get(SERVER_LIST, servers);
		List<ServerData> loaded = Access.get(SERVER_LIST, fresh);
		List<ServerData> hidden = Access.get(HIDDEN_SERVER_LIST, servers);
		List<ServerData> loadedHidden = Access.get(HIDDEN_SERVER_LIST, fresh);
		if (live == null || loaded == null || hidden == null || loadedHidden == null) {
			return;
		}
		List<ServerData> merged = OptionsFile.reconcile(live, loaded, data -> data.ip, ServerData::copyFrom);
		live.clear();
		live.addAll(merged);
		List<ServerData> mergedHidden = OptionsFile.reconcile(hidden, loadedHidden, data -> data.ip, ServerData::copyFrom);
		hidden.clear();
		hidden.addAll(mergedHidden);
		list.updateOnlineServers(servers);
	}

	private void load(Options options, File file, Map<String, String> changes) {
		if (OPTIONS_FILE == null) {
			LOGGER.warn("shared options left for the next start: the options fields are not where the companion expects them");
			return;
		}
		File part = new File(file.getParentFile(), PART_FILE);
		Object startup = Access.get(STARTUP_BACKEND, options);
		applying = true;
		try {
			String version = Integer.toString(SharedConstants.getCurrentVersion().dataVersion().version());
			Files.writeString(part.toPath(), OptionsFile.lines(version, changes), StandardCharsets.UTF_8);
			if (Access.set(OPTIONS_FILE, options, part)) {
				options.load();
			}
		} catch (IOException e) {
			LOGGER.warn("cannot apply shared options: {}", e.toString());
		} finally {
			Access.set(OPTIONS_FILE, options, file);
			if (startup != null) {
				Access.set(STARTUP_BACKEND, options, startup);
			}
			applying = false;
			try {
				Files.deleteIfExists(part.toPath());
			} catch (IOException ignored) {
			}
		}
	}
}
