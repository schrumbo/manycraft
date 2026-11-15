package io.github.schrumbo.manycraft;

import io.github.schrumbo.manycraft.mixin.LibraryAccessor;
import io.github.schrumbo.manycraft.mixin.SoundEngineAccessor;
import io.github.schrumbo.manycraft.mixin.SoundManagerAccessor;
import net.minecraft.client.Minecraft;
import org.lwjgl.openal.ALC;
import org.lwjgl.openal.SOFTPauseDevice;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class AudioPause {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");

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
		SoundEngineAccessor engine = (SoundEngineAccessor) ((SoundManagerAccessor) mc.getSoundManager()).manycraft$soundEngine();
		return engine.manycraft$loaded() ? ((LibraryAccessor) engine.manycraft$library()).manycraft$currentDevice() : 0;
	}
}
