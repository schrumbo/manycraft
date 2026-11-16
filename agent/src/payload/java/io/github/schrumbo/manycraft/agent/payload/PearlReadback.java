package io.github.schrumbo.manycraft.agent.payload;

import com.mojang.blaze3d.pipeline.RenderTarget;
import com.mojang.blaze3d.systems.RenderSystem;
import com.mojang.renderpearl.api.buffers.GpuBuffer;
import com.mojang.renderpearl.api.buffers.GpuBufferSlice;
import com.mojang.renderpearl.api.textures.GpuTexture;
import java.nio.ByteBuffer;
import java.util.function.Consumer;

final class PearlReadback implements Readback {
	@Override
	public boolean start(RenderTarget target, int width, int height, Consumer<ByteBuffer> mapped, Runnable failed) {
		GpuTexture texture = target.getColorTexture();
		if (texture == null || texture.getFormat().blockSize() != 4) {
			return false;
		}
		GpuBuffer buffer = RenderSystem.getDevice().createBuffer(() -> "Manycraft preview", GpuBuffer.USAGE_MAP_READ | GpuBuffer.USAGE_COPY_DST, (long) width * height * 4);
		try {
			RenderSystem.getDevice().createCommandEncoder().copyTextureToBuffer(texture, buffer, 0L, () -> {
				try (GpuBufferSlice.MappedView view = buffer.map(true, false)) {
					mapped.accept(view.data());
				} catch (RuntimeException e) {
					failed.run();
				} finally {
					buffer.close();
				}
			}, 0);
		} catch (RuntimeException e) {
			buffer.close();
			throw e;
		}
		return true;
	}
}
