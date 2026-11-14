package io.github.schrumbo.manycraft;

import io.github.schrumbo.manycraft.mixin.OptionsAccessor;
import java.util.function.Consumer;
import net.minecraft.client.CloudStatus;
import net.minecraft.client.Minecraft;
import net.minecraft.client.OptionInstance;
import net.minecraft.client.Options;
import net.minecraft.server.level.ParticleStatus;
import net.minecraft.sounds.SoundSource;
import net.minecraft.util.Mth;
import org.lwjgl.sdl.SDLVideo;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class BackgroundController {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");

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

	record Values(int renderDistance, int simulationDistance, ParticleStatus particles, CloudStatus clouds, double masterVolume) {
		Values reduced(BackgroundLevel level, int savingRenderDistance, int savingSimulationDistance) {
			return switch (level) {
				case SAVING -> new Values(savingRenderDistance, savingSimulationDistance, ParticleStatus.MINIMAL, CloudStatus.OFF, 0.0);
				case BALANCED -> new Values(renderDistance, simulationDistance, ParticleStatus.MINIMAL, clouds, 0.0);
				case FULL -> new Values(renderDistance, simulationDistance, particles, clouds, 0.0);
			};
		}
	}

	private final Minecraft mc;
	private final SaveGuard guard;
	private final Consumer<String> sink;
	private final BackgroundMode<Values> mode;
	private volatile Profile profile = Profile.DEFAULT;
	private volatile BackgroundLevel level = BackgroundLevel.SAVING;
	private volatile boolean visible = true;
	private boolean lastFocused = true;

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
					o.getSoundSourceOptionInstance(SoundSource.MASTER).get()
				);
			}

			@Override
			public Values background(Values saved) {
				Options o = mc.options;
				return saved.reduced(level, clamp(o.renderDistance(), profile.renderDistance()), clamp(o.simulationDistance(), 0));
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
		return !visible && level.skipsRender();
	}

	int hiddenFramerate() {
		return switch (level) {
			case SAVING -> profile.fps();
			case BALANCED -> Math.min(BackgroundLevel.BALANCED_FPS, mc.options.framerateLimit().get());
			case FULL -> mc.options.framerateLimit().get();
		};
	}

	void applyLevel(BackgroundLevel next) {
		if (next == level) {
			return;
		}
		level = next;
		LOGGER.info("background level {}", next);
		if (!visible) {
			mode.enter();
		}
	}

	boolean blocksSave() {
		return guard.blocksSave();
	}

	void applyProfile(Protocol.Incoming.Profile update) {
		profile = profile.merge(update);
		if (!visible) {
			mode.enter();
		}
	}

	void hide() {
		mode.enter();
		SDLVideo.SDL_HideWindow(mc.getWindow().handle());
		visible = false;
		reportWindow();
	}

	void show(int x, int y, int w, int h, String windowMode) {
		mode.exit();
		GameWindow.show(mc, x, y, w, h, windowMode);
		visible = true;
		mc.getFramerateLimitTracker().onInputReceived();
		reportWindow();
	}

	void startHidden() {
		mode.enter();
		SDLVideo.SDL_HideWindow(mc.getWindow().handle());
		visible = false;
	}

	String windowLine() {
		return Protocol.window(visible, visible && mc.getWindow().isFocused());
	}

	void tick() {
		boolean focused = visible && mc.getWindow().isFocused();
		if (focused != lastFocused) {
			lastFocused = focused;
			reportWindow();
		}
	}

	private void reportWindow() {
		lastFocused = visible && mc.getWindow().isFocused();
		sink.accept(windowLine());
	}

	private static int clamp(OptionInstance<Integer> option, int wanted) {
		var range = (OptionInstance.IntRangeBase) option.values();
		return Mth.clamp(wanted, range.minInclusive(), range.maxInclusive());
	}
}
