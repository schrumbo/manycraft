package io.github.schrumbo.manycraft.agent.payload;

import com.mojang.blaze3d.pipeline.RenderTarget;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.nio.ByteBuffer;
import java.util.function.Consumer;
import java.util.function.Supplier;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class BlazeReadback implements Readback {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");

	private final Method device;
	private final Method createBuffer;
	private final Method createEncoder;
	private final Method copy;
	private final Method colorTexture;
	private final Method bufferMap;
	private final Method encoderMap;
	private final Method data;
	private final int usage;

	private BlazeReadback(Method device, Method createBuffer, Method createEncoder, Method copy, Method colorTexture, Method bufferMap, Method encoderMap, Method data, int usage) {
		this.device = device;
		this.createBuffer = createBuffer;
		this.createEncoder = createEncoder;
		this.copy = copy;
		this.colorTexture = colorTexture;
		this.bufferMap = bufferMap;
		this.encoderMap = encoderMap;
		this.data = data;
		this.usage = usage;
	}

	static Readback create() {
		try {
			ClassLoader loader = BlazeReadback.class.getClassLoader();
			Class<?> system = Class.forName("com.mojang.blaze3d.systems.RenderSystem", false, loader);
			Class<?> gpuDevice = Class.forName("com.mojang.blaze3d.systems.GpuDevice", false, loader);
			Class<?> encoder = Class.forName("com.mojang.blaze3d.systems.CommandEncoder", false, loader);
			Class<?> buffer = Class.forName("com.mojang.blaze3d.buffers.GpuBuffer", false, loader);
			Class<?> texture = Class.forName("com.mojang.blaze3d.textures.GpuTexture", false, loader);
			Method bufferMap = optional(buffer, "map", boolean.class, boolean.class);
			Method encoderMap = bufferMap != null ? null : encoder.getMethod("mapBuffer", buffer, boolean.class, boolean.class);
			Class<?> view = (bufferMap != null ? bufferMap : encoderMap).getReturnType();
			int usage = buffer.getField("USAGE_MAP_READ").getInt(null) | buffer.getField("USAGE_COPY_DST").getInt(null);
			return new BlazeReadback(
				system.getMethod("getDevice"),
				gpuDevice.getMethod("createBuffer", Supplier.class, int.class, long.class),
				gpuDevice.getMethod("createCommandEncoder"),
				encoder.getMethod("copyTextureToBuffer", texture, buffer, long.class, Runnable.class, int.class),
				RenderTarget.class.getMethod("getColorTexture"),
				bufferMap,
				encoderMap,
				view.getMethod("data"),
				usage
			);
		} catch (ReflectiveOperationException | RuntimeException | LinkageError e) {
			LOGGER.warn("no preview readback for this game version, previews stay off: {}", e.toString());
			return (target, width, height, mapped, failed) -> false;
		}
	}

	private static Method optional(Class<?> owner, String name, Class<?>... params) {
		try {
			return owner.getMethod(name, params);
		} catch (NoSuchMethodException e) {
			return null;
		}
	}

	@Override
	public boolean start(RenderTarget target, int width, int height, Consumer<ByteBuffer> mapped, Runnable failed) {
		try {
			Object texture = colorTexture.invoke(target);
			if (texture == null) {
				return false;
			}
			Object gpu = device.invoke(null);
			Supplier<String> label = () -> "Manycraft preview";
			AutoCloseable buffer = (AutoCloseable) createBuffer.invoke(gpu, label, usage, (long) width * height * 4);
			Object encoder = createEncoder.invoke(gpu);
			Runnable done = () -> {
				try (AutoCloseable view = (AutoCloseable) (bufferMap != null ? bufferMap.invoke(buffer, true, false) : encoderMap.invoke(createEncoder.invoke(device.invoke(null)), buffer, true, false))) {
					mapped.accept((ByteBuffer) data.invoke(view));
				} catch (Exception e) {
					failed.run();
				} finally {
					close(buffer);
				}
			};
			try {
				copy.invoke(encoder, texture, buffer, 0L, done, 0);
			} catch (ReflectiveOperationException | RuntimeException e) {
				close(buffer);
				throw e;
			}
			return true;
		} catch (InvocationTargetException e) {
			throw e.getCause() instanceof RuntimeException r ? r : new IllegalStateException(e.getCause());
		} catch (ReflectiveOperationException e) {
			throw new IllegalStateException(e);
		}
	}

	private static void close(AutoCloseable closeable) {
		try {
			closeable.close();
		} catch (Exception e) {
			LOGGER.debug("preview buffer close failed", e);
		}
	}
}
