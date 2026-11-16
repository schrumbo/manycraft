package io.github.schrumbo.manycraft.agent.payload;

import io.github.schrumbo.manycraft.agent.Bridge;
import io.github.schrumbo.manycraft.agent.Hooks;
import io.github.schrumbo.manycraft.agent.Patches;
import io.github.schrumbo.manycraft.agent.Proxies;
import io.netty.channel.Channel;
import java.time.Instant;
import java.util.Optional;
import java.util.Queue;
import java.util.concurrent.ConcurrentLinkedQueue;
import java.util.concurrent.atomic.AtomicBoolean;
import com.mojang.authlib.GameProfile;
import net.minecraft.client.DeltaTracker;
import net.minecraft.client.Minecraft;
import net.minecraft.client.Options;
import net.minecraft.client.gui.GuiGraphicsExtractor;
import net.minecraft.client.gui.screens.ConnectScreen;
import net.minecraft.client.gui.screens.DeathScreen;
import net.minecraft.client.gui.screens.TitleScreen;
import net.minecraft.client.gui.screens.multiplayer.JoinMultiplayerScreen;
import net.minecraft.client.input.KeyEvent;
import net.minecraft.client.multiplayer.ServerData;
import net.minecraft.client.multiplayer.resolver.ResolvedServerAddress;
import net.minecraft.client.multiplayer.resolver.ServerAddress;
import net.minecraft.network.chat.ChatType;
import net.minecraft.network.chat.Component;
import net.minecraft.network.chat.PlayerChatMessage;
import net.minecraft.util.StringUtil;
import org.apache.commons.lang3.StringUtils;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class Companion implements Bridge {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	private static final long SMOKE_WINDOW = 5_000_000_000L;
	private static final long SMOKE_SHOWN_MS = 6000;

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
	private final SaveGuard guard = new SaveGuard(() -> Patches.has("options"), on -> Hooks.guarded = on);
	private final QueueDetector queue = new QueueDetector();
	private final ManagerLink link;
	private final StateReporter state;
	private final ChatBridge chat;
	private final PreviewService preview;
	private final GameIdentity identity = new GameIdentity();
	private final OptionSync options = new OptionSync();
	private final AtomicBoolean closing = new AtomicBoolean();
	private final AudioPause audio = new AudioPause();
	private volatile BackgroundController background;
	private volatile Hotkeys hotkeys = Hotkeys.DEFAULT;
	private final Queue<Protocol.Incoming> early = new ConcurrentLinkedQueue<>();
	private boolean previewDue;
	private final FrameRate smoke = Boolean.getBoolean("manycraft.smoke") ? new FrameRate(SMOKE_WINDOW) : null;
	private final Headless headless = Hooks.headless() ? new Headless() : null;
	private long shownAt;
	private boolean shownDrawn;
	private int shownSections;

	private Companion(Config config, String version) {
		this.config = config;
		this.version = version;
		this.link = new ManagerLink(config, this::handshake, this::onInbound);
		this.state = new StateReporter(this::send, queue);
		this.chat = new ChatBridge(this::send, queue);
		this.preview = config.preview() == null ? null : new PreviewService(config.preview(), this::send);
	}

	static Companion start(String version) {
		Config config;
		try {
			config = Config.read(System::getProperty, System::getenv);
		} catch (RuntimeException e) {
			LOGGER.error("invalid manycraft launch properties, companion disabled", e);
			return null;
		}
		if (config == null) {
			return null;
		}
		Companion companion = new Companion(config, version);
		companion.link.start();
		return companion;
	}

	@Override
	public void frame(Object minecraft) {
		if (background == null) {
			started((Minecraft) minecraft);
		}
		if (smoke != null) {
			BackgroundController bg = background;
			smoke.count(System.nanoTime()).ifPresent(w -> LOGGER.info("manycraft smoke fps {} hidden {} worst {} ms slow {}", Math.round(w.fps()), bg != null && bg.hidden(), w.worstMs(), w.slow()));
		}
	}

	@Override
	public boolean skipFrame(Object minecraft) {
		BackgroundController bg = background;
		if (headless == null || bg == null || !bg.skipsRender() || preview != null && preview.due(bg.profile().previewIntervalMs())) {
			return false;
		}
		Minecraft mc = (Minecraft) minecraft;
		if (!Headless.applies(mc)) {
			return false;
		}
		headless.idle(mc, bg.hiddenFramerate());
		return true;
	}

	@Override
	public boolean managed() {
		return true;
	}

	@Override
	public int hiddenFramerate() {
		BackgroundController bg = background;
		return bg != null && bg.hidden() ? bg.hiddenFramerate() : 0;
	}

	@Override
	public void audioClosed() {
		audio.closed();
	}

	@Override
	public boolean saveOptions(Object target) {
		if (guard.blocksSave()) {
			return true;
		}
		Options gameOptions = (Options) target;
		Runnable original = () -> GameWindow.saving(gameOptions, () -> Hooks.saveOriginal(gameOptions::save));
		var changed = options.save(gameOptions, original);
		if (!changed.isEmpty()) {
			send(Protocol.options(changed));
		}
		return true;
	}

	@Override
	public void serversSaved() {
		send(Protocol.servers());
	}

	@Override
	public void multiplayerOpened(Object screen) {
		options.multiplayerOpened((JoinMultiplayerScreen) screen);
	}

	@Override
	public String title() {
		return identity.title();
	}

	@Override
	public void connecting(Object data) {
		state.noteConnecting((ServerData) data);
	}

	@Override
	public void channel(Object channel) {
		ProxyInstaller.apply((Channel) channel);
	}

	@Override
	public Object remoteName(Object target) {
		ServerAddress address = (ServerAddress) target;
		var named = Proxies.remoteName(address.getHost(), address.getPort());
		return named == null ? null : Optional.of(ResolvedServerAddress.from(named));
	}

	@Override
	public void systemChat(Object message, boolean overlay) {
		chat.onGame((Component) message, overlay);
	}

	@Override
	public void playerChat(Object bound, Object signed, Object decorated, Object sender, Object receivedAt) {
		chat.onChat((Component) decorated, (PlayerChatMessage) signed, (GameProfile) sender, (ChatType.Bound) bound, (Instant) receivedAt);
	}

	@Override
	public void hud(Object graphics, Object delta) {
		identity.extractBadge((GuiGraphicsExtractor) graphics, (DeltaTracker) delta);
	}

	@Override
	public boolean beforeRender(Object renderer) {
		Minecraft mc = Minecraft.getInstance();
		GameWindow.frame(mc);
		previewDue = false;
		BackgroundController bg = background;
		if (bg == null) {
			return true;
		}
		BackgroundController.Profile p = bg.profile();
		previewDue = preview != null && preview.due(p.previewIntervalMs());
		return previewDue || !bg.skipsRender();
	}

	@Override
	public void afterRender(Object renderer) {
		BackgroundController bg = background;
		if (shownAt != 0 && bg != null && !bg.hidden()) {
			smokeShown();
		}
		if (previewDue && bg != null) {
			previewDue = false;
			preview.capture(Compat.mainTarget(Minecraft.getInstance()), bg.profile().previewWidth());
		}
	}

	private void smokeShown() {
		long ms = (System.nanoTime() - shownAt) / 1_000_000;
		if (!shownDrawn) {
			shownDrawn = true;
			LOGGER.info("manycraft smoke first frame {} ms after show", ms);
		}
		int sections = Compat.renderedSections(Minecraft.getInstance());
		if (sections != shownSections) {
			shownSections = sections;
			LOGGER.info("manycraft smoke {} sections {} ms after show", sections, ms);
		}
		if (ms > SMOKE_SHOWN_MS) {
			shownAt = 0;
		}
	}

	@Override
	public boolean key(int action, Object target) {
		KeyEvent event = (KeyEvent) target;
		int keycode = Compat.shortcutKey(event);
		int modifiers = event.modifiers();
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

	@Override
	public void closeRequested() {
		if (closing.compareAndSet(false, true)) {
			send(Protocol.closing());
		}
	}

	private void started(Minecraft mc) {
		BackgroundController bg = new BackgroundController(mc, guard, this::send);
		background = bg;
		if (config.hidden()) {
			bg.startHidden();
		}
	}

	@Override
	public void tick(Object minecraft) {
		Minecraft mc = (Minecraft) minecraft;
		try {
			link.ensureRunning();
			drain(mc);
			state.tick(mc);
			GameWindow.tick();
			BackgroundController bg = background;
			if (bg != null) {
				bg.tick();
			}
			audio.sync(mc, bg != null && bg.hidden());
		} catch (RuntimeException e) {
			LOGGER.warn("tick failed", e);
		}
	}

	private String[] handshake() {
		BackgroundController bg = background;
		return new String[] {
			Protocol.hello(config.instance(), config.token(), ProcessHandle.current().pid(), version, Patches.applied()),
			state.line(),
			bg == null ? Protocol.window(!config.hidden(), false) : bg.windowLine()
		};
	}

	private void send(String line) {
		link.send(line);
	}

	private void onInbound(Protocol.Incoming message) {
		early.add(message);
		Minecraft mc = Minecraft.getInstance();
		if (mc != null) {
			mc.execute(() -> drain(mc));
		}
	}

	private void drain(Minecraft mc) {
		Protocol.Incoming next;
		while ((next = early.peek()) != null && (background != null || !needsBackground(next))) {
			early.poll();
			try {
				handle(mc, next);
			} catch (RuntimeException e) {
				LOGGER.warn("manager message failed", e);
			}
		}
	}

	private static boolean needsBackground(Protocol.Incoming message) {
		return !(message instanceof Protocol.Incoming.Hotkeys || message instanceof Protocol.Incoming.Quit || message instanceof Protocol.Incoming.Identity || message instanceof Protocol.Incoming.Badge || message instanceof Protocol.Incoming.Hint || message instanceof Protocol.Incoming.Options || message instanceof Protocol.Incoming.Servers);
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
		if (message instanceof Protocol.Incoming.Hint h) {
			identity.showHint(h.text());
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
				shownAt = smoke == null ? 0 : System.nanoTime();
				shownDrawn = false;
				shownSections = -1;
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
		if (data == null || mc.getConnection() != null || Compat.screen(mc) instanceof ConnectScreen) {
			return;
		}
		ConnectScreen.startConnecting(new TitleScreen(), mc, ServerAddress.parseString(data.ip), data, false, null);
	}

	private void respawn(Minecraft mc) {
		if (mc.player == null || !(Compat.screen(mc) instanceof DeathScreen)) {
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
