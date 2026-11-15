package io.github.schrumbo.manycraft;

import java.util.function.Consumer;
import net.minecraft.client.Minecraft;
import net.minecraft.client.Options;
import net.minecraft.client.gui.screens.ConnectScreen;
import net.minecraft.client.gui.screens.DeathScreen;
import net.minecraft.client.gui.screens.TitleScreen;
import net.minecraft.client.gui.screens.multiplayer.JoinMultiplayerScreen;
import net.minecraft.client.multiplayer.ServerData;
import net.minecraft.client.multiplayer.resolver.ServerAddress;
import net.minecraft.client.renderer.GameRenderer;
import net.minecraft.util.StringUtil;
import org.apache.commons.lang3.StringUtils;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class Companion {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	private static volatile Companion instance;

	record Hotkeys(KeyChord next, KeyChord prev, KeyChord nextAttention, KeyChord grid) {
		static final Hotkeys DEFAULT = new Hotkeys(
			KeyChord.parse("ctrl+alt+right"),
			KeyChord.parse("ctrl+alt+left"),
			KeyChord.parse("ctrl+alt+down"),
			KeyChord.parse("ctrl+alt+g")
		);
	}

	private final Config config;
	private final String version;
	private final SaveGuard guard = new SaveGuard();
	private final QueueDetector queue = new QueueDetector();
	private final ManagerLink link;
	private final StateReporter state;
	private final ChatBridge chat;
	private final PreviewService preview;
	private final GameIdentity identity = new GameIdentity();
	private final OptionSync options = new OptionSync();
	private volatile BackgroundController background;
	private volatile Hotkeys hotkeys = Hotkeys.DEFAULT;

	private Companion(Config config, String version) {
		this.config = config;
		this.version = version;
		this.link = new ManagerLink(config, this::handshake, this::onInbound);
		this.state = new StateReporter(this::send, queue);
		this.chat = new ChatBridge(this::send, queue);
		this.preview = config.preview() == null ? null : new PreviewService(config.preview(), this::send);
	}

	static Companion get() {
		return instance;
	}

	static void start(String version, Consumer<Companion> events) {
		Config config;
		try {
			config = Config.read(System::getProperty, System::getenv);
		} catch (RuntimeException e) {
			LOGGER.error("invalid manycraft launch properties, companion disabled", e);
			return;
		}
		if (config == null) {
			return;
		}
		ProxyInstaller.installDefaults();
		Companion companion = new Companion(config, version);
		instance = companion;
		events.accept(companion);
		companion.link.start();
	}

	ChatBridge chat() {
		return chat;
	}

	GameIdentity identity() {
		return identity;
	}

	boolean blocksSave() {
		return guard.blocksSave();
	}

	boolean hidden() {
		BackgroundController bg = background;
		return bg != null && bg.hidden();
	}

	int hiddenFramerate() {
		return background.hiddenFramerate();
	}

	boolean skipsAmbience() {
		BackgroundController bg = background;
		return bg != null && bg.skipsAmbience();
	}

	void saveOptions(Options gameOptions, Runnable save) {
		if (guard.blocksSave()) {
			save.run();
			return;
		}
		var changed = options.save(gameOptions, save);
		if (!changed.isEmpty()) {
			send(Protocol.options(changed));
		}
	}

	void serversSaved() {
		send(Protocol.servers());
	}

	void multiplayerOpened(JoinMultiplayerScreen screen) {
		options.multiplayerOpened(screen);
	}

	String title() {
		return identity.title();
	}

	void noteConnecting(ServerData data) {
		state.noteConnecting(data);
	}

	void render(GameRenderer renderer, Runnable body) {
		GameWindow.frame(Minecraft.getInstance());
		BackgroundController bg = background;
		if (bg == null) {
			body.run();
			return;
		}
		BackgroundController.Profile p = bg.profile();
		boolean due = preview != null && preview.due(p.previewIntervalMs());
		if (bg.skipsRender() && !due) {
			return;
		}
		body.run();
		if (due) {
			preview.capture(Minecraft.getInstance().gameRenderer.mainRenderTarget(), p.previewWidth());
		}
	}

	boolean onKey(int action, int keycode, int modifiers) {
		Hotkeys keys = hotkeys;
		String name = null;
		if (keys.next().matches(keycode, modifiers)) {
			name = "next";
		} else if (keys.prev().matches(keycode, modifiers)) {
			name = "prev";
		} else if (keys.nextAttention().matches(keycode, modifiers)) {
			name = "next_attention";
		} else if (keys.grid().matches(keycode, modifiers)) {
			name = "grid";
		}
		if (name == null) {
			return false;
		}
		if (action == 1) {
			GameWindow.grantForeground();
			send(Protocol.hotkey(name));
		}
		return true;
	}

	boolean closeToGrid() {
		BackgroundController bg = background;
		if (bg == null || bg.hidden() || !GameWindow.shown(Minecraft.getInstance())) {
			return false;
		}
		GameWindow.grantForeground();
		return link.send(Protocol.hotkey("grid"));
	}

	void started(Minecraft mc) {
		BackgroundController bg = new BackgroundController(mc, guard, this::send);
		background = bg;
		if (config.hidden()) {
			bg.startHidden();
		}
	}

	void tick(Minecraft mc) {
		try {
			link.ensureRunning();
			state.tick(mc);
			GameWindow.tick();
			BackgroundController bg = background;
			if (bg != null) {
				bg.tick();
			}
		} catch (RuntimeException e) {
			LOGGER.warn("tick failed", e);
		}
	}

	private String[] handshake() {
		BackgroundController bg = background;
		return new String[] {
			Protocol.hello(config.instance(), config.token(), ProcessHandle.current().pid(), version),
			state.line(),
			bg == null ? Protocol.window(!config.hidden(), false) : bg.windowLine()
		};
	}

	private void send(String line) {
		link.send(line);
	}

	private void onInbound(Protocol.Incoming message) {
		Minecraft.getInstance().execute(() -> handle(Minecraft.getInstance(), message));
	}

	private void handle(Minecraft mc, Protocol.Incoming message) {
		if (message instanceof Protocol.Incoming.Hotkeys keys) {
			hotkeys = merge(hotkeys, keys);
			return;
		}
		if (message instanceof Protocol.Incoming.Quit) {
			mc.stop();
			return;
		}
		if (message instanceof Protocol.Incoming.Identity id) {
			identity.apply(mc, id);
			return;
		}
		if (message instanceof Protocol.Incoming.Badge b) {
			identity.showBadge(b.visible());
			return;
		}
		if (message instanceof Protocol.Incoming.Options shared) {
			options.apply(mc, shared.values(), background);
			return;
		}
		if (message instanceof Protocol.Incoming.Servers) {
			options.serversChanged(mc);
			return;
		}
		BackgroundController bg = background;
		if (bg == null) {
			return;
		}
		switch (message) {
			case Protocol.Incoming.Show show -> {
				bg.show(show.x(), show.y(), show.w(), show.h(), show.mode());
				options.shown();
			}
			case Protocol.Incoming.Hide hide -> bg.hide();
			case Protocol.Incoming.Profile profile -> bg.applyProfile(profile);
			case Protocol.Incoming.Level level -> bg.applyLevel(level.level(), level.custom());
			case Protocol.Incoming.ChatSend chatSend -> sendChat(mc, chatSend.text());
			case Protocol.Incoming.Reconnect reconnect -> reconnect(mc);
			case Protocol.Incoming.Respawn respawn -> respawn(mc);
			default -> {
			}
		}
	}

	private void sendChat(Minecraft mc, String text) {
		var connection = mc.getConnection();
		String clean = StringUtil.trimChatMessage(StringUtil.filterText(StringUtils.normalizeSpace(text.trim())));
		if (connection == null || mc.player == null || clean.isEmpty()) {
			return;
		}
		if (clean.startsWith("/")) {
			connection.sendCommand(clean.substring(1));
		} else {
			connection.sendChat(clean);
		}
	}

	private void reconnect(Minecraft mc) {
		ServerData data = state.lastServer();
		if (data == null || mc.getConnection() != null || mc.gui.screen() instanceof ConnectScreen) {
			return;
		}
		ConnectScreen.startConnecting(new TitleScreen(), mc, ServerAddress.parseString(data.ip), data, false, null);
	}

	private void respawn(Minecraft mc) {
		if (mc.player == null || !(mc.gui.screen() instanceof DeathScreen)) {
			return;
		}
		mc.player.respawn();
	}

	private static Hotkeys merge(Hotkeys current, Protocol.Incoming.Hotkeys update) {
		return new Hotkeys(
			chord(update.next(), current.next()),
			chord(update.prev(), current.prev()),
			chord(update.nextAttention(), current.nextAttention()),
			chord(update.grid(), current.grid())
		);
	}

	private static KeyChord chord(String spec, KeyChord fallback) {
		if (spec == null) {
			return fallback;
		}
		try {
			return KeyChord.parse(spec);
		} catch (IllegalArgumentException e) {
			LOGGER.warn("ignoring hotkey: {}", e.getMessage());
			return fallback;
		}
	}
}
