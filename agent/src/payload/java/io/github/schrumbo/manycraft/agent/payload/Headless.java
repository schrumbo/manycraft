package io.github.schrumbo.manycraft.agent.payload;

import com.mojang.blaze3d.systems.RenderSystem;
import java.util.concurrent.locks.LockSupport;
import net.minecraft.client.Minecraft;

final class Headless {
	private final FramePacer pacer = new FramePacer();

	static boolean applies(Minecraft mc) {
		return mc.level != null && mc.player != null && Compat.screen(mc) == null && !Compat.overlay(mc);
	}

	void idle(Minecraft mc, int fps) {
		mc.level.update();
		RenderSystem.getDevice().createCommandEncoder().submit();
		RenderSystem.executePendingTasks();
		long wait = pacer.next(System.nanoTime(), fps);
		if (wait > 0) {
			LockSupport.parkNanos(wait);
		}
	}

	static final class FramePacer {
		private long due;

		long next(long now, int fps) {
			long period = 1_000_000_000L / Math.max(1, fps);
			due = due == 0 || now - due > period ? now + period : due + period;
			return due - now;
		}
	}
}
