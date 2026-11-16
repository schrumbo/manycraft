package io.github.schrumbo.manycraft.agent.payload;

import com.mojang.blaze3d.pipeline.RenderTarget;
import com.mojang.blaze3d.systems.RenderSystem;
import com.mojang.renderpearl.api.buffers.GpuBuffer;
import com.mojang.renderpearl.api.buffers.GpuBufferSlice;
import com.mojang.renderpearl.api.commands.CommandEncoder;
import com.mojang.renderpearl.api.textures.GpuTexture;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.nio.ByteBuffer;
import java.util.function.Consumer;

final class PearlReadback implements Readback {
	private static final Method FENCED_COPY = fencedCopy();

	@Override
	public boolean start(RenderTarget target, int width, int height, Consumer<ByteBuffer> mapped, Runnable failed) {
		GpuTexture texture = target.getColorTexture();
		if (texture == null || texture.getFormat().blockSize() != 4) {
			return false;
		}
		GpuBuffer buffer = RenderSystem.getDevice().createBuffer(() -> "Manycraft preview", GpuBuffer.USAGE_MAP_READ | GpuBuffer.USAGE_COPY_DST, (long) width * height * 4);
		Runnable done = () -> {
			try (GpuBufferSlice.MappedView view = buffer.map(true, false)) {
				mapped.accept(view.data());
			} catch (RuntimeException e) {
				failed.run();
			} finally {
				buffer.close();
			}
		};
		try {
			CommandEncoder encoder = RenderSystem.getDevice().createCommandEncoder();
			if (FENCED_COPY == null) {
				encoder.copyTextureToBuffer(texture, buffer, 0L, done, 0);
			} else {
				FENCED_COPY.invoke(encoder, texture, buffer, 0L, 0);
				RenderSystem.queueFencedTask(done);
			}
		} catch (RuntimeException e) {
			buffer.close();
			throw e;
		} catch (ReflectiveOperationException e) {
			buffer.close();
			throw new IllegalStateException(e instanceof InvocationTargetException i ? i.getCause() : e);
		}
		return true;
	}

	private static Method fencedCopy() {
		try {
			return CommandEncoder.class.getMethod("copyTextureToBuffer", GpuTexture.class, GpuBuffer.class, long.class, int.class);
		} catch (NoSuchMethodException e) {
			return null;
		}
	}
}
