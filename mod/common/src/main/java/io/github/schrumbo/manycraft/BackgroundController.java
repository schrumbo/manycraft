package io.github.schrumbo.manycraft;

import com.mojang.serialization.JsonOps;
import io.github.schrumbo.manycraft.mixin.OptionsAccessor;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Set;
import java.util.function.Consumer;
import net.minecraft.client.CloudStatus;
import net.minecraft.client.Minecraft;
import net.minecraft.client.OptionInstance;
import net.minecraft.client.Options;
import net.minecraft.server.level.ParticleStatus;
import net.minecraft.sounds.SoundSource;
import net.minecraft.util.LenientJsonParser;
import net.minecraft.util.Mth;
import org.lwjgl.sdl.SDLVideo;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class BackgroundController {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");
	private static final int SHRINK_TICKS = 600;
	private static final int TRIM_TICKS = 600;
	private static final String MASTER_VOLUME = "soundCategory_" + SoundSource.MASTER.getName();
	static final Set<String> USER_KEYS = Set.of("renderDistance", "simulationDistance", "particles", "renderClouds", MASTER_VOLUME, "maxFps");

	static final int PREVIEW_FPS_MAX = 10;

	record Profile(int fps, int renderDistance, int previewIntervalMs, int previewWidth) {
		static final Profile DEFAULT = new Profile(3, 2, 3000, 320);

		Profile merge(Protocol.Incoming.Profile update) {
			return new Profile(
				clampMin(update.fps(), fps, 1),
				clampMin(update.renderDistance(), renderDistance, 2),
				clampMin(update.previewIntervalMs(), previewIntervalMs, 250),
				clampMin(update.previewWidth(), previewWidth, 16)
			);
		}

		private static int clampMin(Integer value, int fallback, int min) {
			return Math.max(min, value == null ? fallback : value);
		}
	}

	record Values(int renderDistance, int simulationDistance, ParticleStatus particles, CloudStatus clouds, double masterVolume, int framerateLimit) {
		Values reduced(BackgroundLevel level, int hiddenRenderDistance, int hiddenSimulationDistance, int hiddenFramerateLimit) {
			return switch (level) {
				case SAVING -> new Values(hiddenRenderDistance, hiddenSimulationDistance, ParticleStatus.MINIMAL, CloudStatus.OFF, 0.0, framerateLimit);
				case BALANCED -> new Values(renderDistance, simulationDistance, ParticleStatus.MINIMAL, clouds, 0.0, framerateLimit);
				case FULL -> new Values(renderDistance, simulationDistance, particles, clouds, 0.0, framerateLimit);
				case CUSTOM -> new Values(Math.min(renderDistance, hiddenRenderDistance), hiddenSimulationDistance, ParticleStatus.MINIMAL, CloudStatus.OFF, 0.0, Math.min(framerateLimit, hiddenFramerateLimit));
			};
		}
	}

	private final Minecraft mc;
	private final SaveGuard guard;
	private final Consumer<String> sink;
	private final BackgroundMode<Values> mode;
	private final HiddenMemory memory = HiddenMemory.create();
	private volatile Profile profile = Profile.DEFAULT;
	private volatile BackgroundLevel level = BackgroundLevel.SAVING;
	private volatile BackgroundLevel.Custom custom = BackgroundLevel.Custom.DEFAULT;
	private volatile boolean visible = true;
	private boolean lastFocused = true;
	private boolean lastMaximized;
	private int hiddenTicks;
	private int settledTicks;

	BackgroundController(Minecraft mc, SaveGuard guard, Consumer<String> sink) {
		this.mc = mc;
		this.guard = guard;
		this.sink = sink;
		this.mode = new BackgroundMode<>(new BackgroundMode.Target<>() {
			@Override
			public Values read() {
				Options o = mc.options;
				return new Values(
					o.renderDistance().get(),
					o.simulationDistance().get(),
					o.particles().get(),
					o.cloudStatus().get(),
					o.getSoundSourceOptionInstance(SoundSource.MASTER).get(),
					o.framerateLimit().get()
				);
			}

			@Override
			public Values background(Values saved) {
				Options o = mc.options;
				BackgroundLevel.Custom c = custom;
				int sight = level == BackgroundLevel.CUSTOM ? c.renderDistance() : profile.renderDistance();
				return saved.reduced(level, clamp(o.renderDistance(), sight), clamp(o.simulationDistance(), 0), BackgroundLevel.optionFramerate(c.fps()));
			}

			@Override
			public void write(Values v) {
				LOGGER.info("options set to {}", v);
				Options o = mc.options;
				OptionsAccessor accessor = (OptionsAccessor) o;
				accessor.manycraft$setApplyingGraphicsPreset(true);
				try {
					o.renderDistance().set(v.renderDistance());
					o.simulationDistance().set(v.simulationDistance());
					o.particles().set(v.particles());
					o.cloudStatus().set(v.clouds());
					o.framerateLimit().set(v.framerateLimit());
				} finally {
					accessor.manycraft$setApplyingGraphicsPreset(false);
				}
				o.getSoundSourceOptionInstance(SoundSource.MASTER).set(v.masterVolume());
			}
		}, guard);
	}

	boolean hidden() {
		return !visible;
	}

	Profile profile() {
		return profile;
	}

	boolean skipsRender() {
		return !visible && level.skipsRender(custom);
	}

	int hiddenFramerate() {
		int base = switch (level) {
			case SAVING -> profile.fps();
			case BALANCED -> Math.min(BackgroundLevel.BALANCED_FPS, mc.options.framerateLimit().get());
			case FULL -> mc.options.framerateLimit().get();
			case CUSTOM -> Math.min(custom.fps(), mc.options.framerateLimit().get());
		};
		return level.skipsRender(custom) ? Math.max(base, previewFramerate(profile.previewIntervalMs())) : base;
	}

	/** Frames a game that only draws for previews needs to keep up with {@code intervalMs}: one to capture, one to read back. */
	static int previewFramerate(int intervalMs) {
		return Math.min(PREVIEW_FPS_MAX, (2000 + intervalMs - 1) / Math.max(1, intervalMs));
	}

	void applyLevel(BackgroundLevel next, BackgroundLevel.Custom nextCustom) {
		if (next == level && (next != BackgroundLevel.CUSTOM || nextCustom.equals(custom))) {
			return;
		}
		custom = nextCustom;
		level = next;
		LOGGER.info("background level {} {}", next, nextCustom);
		if (!visible) {
			mode.enter();
			capWorkers();
		}
	}

	boolean blocksSave() {
		return guard.blocksSave();
	}

	void applyUser(Map<String, String> changes, Consumer<Map<String, String>> load) {
		if (!mode.active()) {
			load.accept(changes);
			return;
		}
		Map<String, String> live = new LinkedHashMap<>();
		Map<String, String> stored = new LinkedHashMap<>();
		changes.forEach((key, value) -> (USER_KEYS.contains(key) ? stored : live).put(key, value));
		if (!live.isEmpty()) {
			load.accept(live);
		}
		mode.store(saved -> parse(saved, stored));
		capWorkers();
	}

	private Values parse(Values saved, Map<String, String> raw) {
		Options o = mc.options;
		return new Values(
			parse(o.renderDistance(), raw.get("renderDistance"), saved.renderDistance()),
			parse(o.simulationDistance(), raw.get("simulationDistance"), saved.simulationDistance()),
			parse(o.particles(), raw.get("particles"), saved.particles()),
			parse(o.cloudStatus(), raw.get("renderClouds"), saved.clouds()),
			parse(o.getSoundSourceOptionInstance(SoundSource.MASTER), raw.get(MASTER_VOLUME), saved.masterVolume()),
			parse(o.framerateLimit(), raw.get("maxFps"), saved.framerateLimit())
		);
	}

	private static <T> T parse(OptionInstance<T> option, String raw, T fallback) {
		if (raw == null || raw.isEmpty()) {
			return fallback;
		}
		try {
			return option.codec().parse(JsonOps.INSTANCE, LenientJsonParser.parse(raw)).result().orElse(fallback);
		} catch (RuntimeException e) {
			return fallback;
		}
	}

	void applyProfile(Protocol.Incoming.Profile update) {
		profile = profile.merge(update);
		if (!visible) {
			mode.enter();
			capWorkers();
		}
	}

	void hide() {
		GameWindow.cancel();
		mode.enter();
		capWorkers();
		SDLVideo.SDL_HideWindow(mc.getWindow().handle());
		visible = false;
		hiddenTicks = 0;
		settledTicks = 0;
		reportWindow();
	}

	void show(int x, int y, int w, int h, String windowMode) {
		boolean released = memory.restore();
		mode.exit();
		if (released) {
			rebuild();
		}
		mc.getFramerateLimitTracker().onInputReceived();
		visible = true;
		GameWindow.show(mc, x, y, w, h, windowMode, this::reportWindow);
	}

	void startHidden() {
		mode.enter();
		capWorkers();
		SDLVideo.SDL_HideWindow(mc.getWindow().handle());
		visible = false;
	}

	String windowLine() {
		boolean shown = visible && !GameWindow.revealing();
		return Protocol.window(shown, shown && mc.getWindow().isFocused());
	}

	void tick() {
		if (!visible && hiddenTicks < SHRINK_TICKS && ++hiddenTicks == SHRINK_TICKS) {
			GameWindow.shrink(mc);
		}
		if (visible || mc.level == null) {
			settledTicks = 0;
		} else if (settledTicks < TRIM_TICKS && ++settledTicks == TRIM_TICKS) {
			memory.trim();
		}
		boolean focused = visible && mc.getWindow().isFocused();
		boolean maximized = visible && maximized();
		if (focused != lastFocused || maximized != lastMaximized) {
			reportWindow();
		}
	}

	private void capWorkers() {
		Values saved = mode.saved();
		if (memory.capWorkers(saved != null && mc.options.renderDistance().get() < saved.renderDistance())) {
			rebuild();
		}
	}

	private void rebuild() {
		mc.levelExtractor.allChanged();
	}

	private boolean maximized() {
		return (SDLVideo.SDL_GetWindowFlags(mc.getWindow().handle()) & SDLVideo.SDL_WINDOW_MAXIMIZED) != 0;
	}

	private void reportWindow() {
		lastFocused = visible && mc.getWindow().isFocused();
		lastMaximized = visible && maximized();
		sink.accept(windowLine());
	}

	private static int clamp(OptionInstance<Integer> option, int wanted) {
		var range = (OptionInstance.IntRangeBase) option.values();
		return Mth.clamp(wanted, range.minInclusive(), range.maxInclusive());
	}
}
