package io.github.schrumbo.manycraft.agent.payload;

import com.mojang.blaze3d.audio.Library;
import java.lang.reflect.Field;
import net.minecraft.client.sounds.SoundEngine;
import net.minecraft.client.sounds.SoundManager;
import net.minecraft.client.Minecraft;
import org.lwjgl.openal.ALC;
import org.lwjgl.openal.SOFTPauseDevice;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class AudioPause {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");

	private static final Field SOUND_ENGINE = Access.field(SoundManager.class, "soundEngine");
	private static final Field LIBRARY = Access.field(SoundEngine.class, "library");
	private static final Field LOADED = Access.field(SoundEngine.class, "loaded");
	private static final Field CURRENT_DEVICE = Access.field(Library.class, "currentDevice");

	private long paused;
	private long unsupported;

	void sync(Minecraft mc, boolean hidden) {
		long device = device(mc);
		if (paused != 0 && paused != device) {
			paused = 0;
		}
		if (hidden && paused == 0 && device != 0 && device != unsupported) {
			if (!ALC.createCapabilities(device).ALC_SOFT_pause_device) {
				unsupported = device;
				return;
			}
			SOFTPauseDevice.alcDevicePauseSOFT(device);
			paused = device;
			LOGGER.info("audio paused while hidden");
		} else if (!hidden && paused != 0) {
			SOFTPauseDevice.alcDeviceResumeSOFT(paused);
			paused = 0;
			LOGGER.info("audio resumed");
		}
	}

	void closed() {
		paused = 0;
		unsupported = 0;
	}

	private static long device(Minecraft mc) {
		SoundEngine engine = Access.get(SOUND_ENGINE, mc.getSoundManager());
		Boolean loaded = engine == null ? null : Access.get(LOADED, engine);
		Library library = Boolean.TRUE.equals(loaded) ? Access.get(LIBRARY, engine) : null;
		Long device = library == null ? null : Access.get(CURRENT_DEVICE, library);
		return device == null ? 0 : device;
	}
}
