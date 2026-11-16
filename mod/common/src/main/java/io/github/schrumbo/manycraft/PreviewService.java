package io.github.schrumbo.manycraft;

import com.mojang.blaze3d.pipeline.RenderTarget;
import com.mojang.blaze3d.systems.RenderSystem;
import com.mojang.renderpearl.api.buffers.GpuBuffer;
import com.mojang.renderpearl.api.buffers.GpuBufferSlice;
import com.mojang.renderpearl.api.textures.GpuTexture;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.file.Path;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.function.Consumer;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

final class PreviewService {
	private static final Logger LOGGER = LoggerFactory.getLogger("manycraft");

	private final Path path;
	private final Consumer<String> sink;
	private final PreviewSchedule schedule = new PreviewSchedule();
	private final AtomicBoolean inFlight = new AtomicBoolean();
	private final ExecutorService worker = Executors.newSingleThreadExecutor(r -> {
		Thread t = new Thread(r, "manycraft-preview");
		t.setDaemon(true);
		return t;
	});
	private PreviewFile file;
	private ByteBuffer staging;
	private byte[] pixels;

	PreviewService(Path path, Consumer<String> sink) {
		this.path = path;
		this.sink = sink;
	}

	boolean due(int intervalMs, int frameMs) {
		return !inFlight.get() && schedule.due(System.nanoTime(), intervalMs, frameMs);
	}

	void soon() {
		schedule.soon();
	}

	void capture(RenderTarget target, int previewWidth) {
		GpuTexture texture = target.getColorTexture();
		int w = target.width;
		int h = target.height;
		if (texture == null || w <= 0 || h <= 0 || texture.getFormat().blockSize() != 4 || !inFlight.compareAndSet(false, true)) {
			return;
		}
		schedule.mark(System.nanoTime());
		try {
			GpuBuffer buffer = RenderSystem.getDevice().createBuffer(() -> "Manycraft preview", GpuBuffer.USAGE_MAP_READ | GpuBuffer.USAGE_COPY_DST, (long) w * h * 4);
			RenderSystem.getDevice().createCommandEncoder().copyTextureToBuffer(texture, buffer, 0L, () -> {
				ByteBuffer frame = null;
				try (GpuBufferSlice.MappedView view = buffer.map(true, false)) {
					int bytes = w * h * 4;
					if (staging == null || staging.capacity() < bytes) {
						staging = ByteBuffer.allocateDirect(bytes);
					}
					staging.clear().limit(bytes);
					ByteBuffer data = view.data();
					staging.put(data.duplicate().position(0).limit(bytes));
					staging.flip();
					frame = staging;
				} catch (RuntimeException e) {
					LOGGER.warn("preview readback failed", e);
				} finally {
					buffer.close();
				}
				if (frame == null) {
					inFlight.set(false);
					return;
				}
				ByteBuffer ready = frame;
				worker.execute(() -> publish(ready, w, h, previewWidth));
			}, 0);
		} catch (RuntimeException e) {
			inFlight.set(false);
			LOGGER.warn("preview capture failed", e);
		}
	}

	private void publish(ByteBuffer frame, int srcW, int srcH, int previewWidth) {
		try {
			int[] size = Downscaler.targetSize(srcW, srcH, previewWidth);
			if (pixels == null || pixels.length < size[0] * size[1] * 4) {
				pixels = new byte[size[0] * size[1] * 4];
			}
			Downscaler.boxFlipped(frame, srcW, srcH, size[0], size[1], pixels);
			if (file == null) {
				file = new PreviewFile(path);
			}
			int seq = file.write(size[0], size[1], pixels);
			sink.accept(Protocol.preview(seq, size[0], size[1]));
		} catch (IOException | RuntimeException e) {
			LOGGER.warn("preview write failed", e);
		} finally {
			inFlight.set(false);
		}
	}
}
