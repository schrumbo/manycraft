package io.github.schrumbo.manycraft.agent.payload;

import com.mojang.blaze3d.pipeline.RenderTarget;
import java.nio.ByteBuffer;
import java.util.function.Consumer;

interface Readback {
	/**
	 * Copies the color texture of {@code target} to a mapped buffer and hands its bytes to {@code mapped} on the render
	 * thread once the GPU is done; returns false when nothing was queued.
	 */
	boolean start(RenderTarget target, int width, int height, Consumer<ByteBuffer> mapped, Runnable failed);

	static Readback forGame() {
		try {
			Class.forName("com.mojang.renderpearl.api.buffers.GpuBuffer", false, Readback.class.getClassLoader());
			return new PearlReadback();
		} catch (ClassNotFoundException | LinkageError e) {
			return BlazeReadback.create();
		}
	}
}
