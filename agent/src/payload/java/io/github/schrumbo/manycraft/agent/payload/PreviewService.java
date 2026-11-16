package io.github.schrumbo.manycraft.agent.payload;

import com.mojang.blaze3d.pipeline.RenderTarget;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.file.Path;
import java.util.Arrays;
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
	private final Readback readback = Readback.forGame();
	private final ExecutorService worker = Executors.newSingleThreadExecutor(r -> {
		Thread t = new Thread(r, "manycraft-preview");
		t.setDaemon(true);
		return t;
	});
	private PreviewFile file;
	private byte[] frame;
	private byte[] pixels;
	private byte[] shown;
	private int shownW;
	private int shownH;

	PreviewService(Path path, Consumer<String> sink) {
		this.path = path;
		this.sink = sink;
	}

	boolean due(int intervalMs) {
		return !inFlight.get() && schedule.due(System.nanoTime(), intervalMs);
	}

	void capture(RenderTarget target, int previewWidth) {
		if (target == null) {
			return;
		}
		int w = target.width;
		int h = target.height;
		if (w <= 0 || h <= 0 || !inFlight.compareAndSet(false, true)) {
			return;
		}
		schedule.mark(System.nanoTime());
		try {
			if (!readback.start(target, w, h, data -> copied(data, w, h, previewWidth), this::readbackFailed)) {
				inFlight.set(false);
			}
		} catch (RuntimeException e) {
			inFlight.set(false);
			LOGGER.warn("preview capture failed", e);
		}
	}

	private void readbackFailed() {
		LOGGER.warn("preview readback failed");
		inFlight.set(false);
	}

	private void copied(ByteBuffer data, int w, int h, int previewWidth) {
		int bytes = w * h * 4;
		if (data.capacity() < bytes) {
			inFlight.set(false);
			return;
		}
		if (frame == null || frame.length < bytes) {
			frame = new byte[bytes];
		}
		data.get(0, frame, 0, bytes);
		byte[] ready = frame;
		worker.execute(() -> publish(ready, w, h, previewWidth));
	}

	private void publish(byte[] frame, int srcW, int srcH, int previewWidth) {
		try {
			int[] size = Downscaler.targetSize(srcW, srcH, previewWidth);
			int bytes = size[0] * size[1] * 4;
			if (pixels == null || pixels.length < bytes) {
				pixels = new byte[bytes];
			}
			Downscaler.boxFlipped(frame, srcW, srcH, size[0], size[1], pixels);
			if (shown != null && shownW == size[0] && shownH == size[1] && Arrays.equals(pixels, 0, bytes, shown, 0, bytes)) {
				return;
			}
			if (file == null) {
				file = new PreviewFile(path);
			}
			int seq = file.write(size[0], size[1], pixels);
			sink.accept(Protocol.preview(seq, size[0], size[1]));
			byte[] spare = shown;
			shown = pixels;
			pixels = spare;
			shownW = size[0];
			shownH = size[1];
		} catch (IOException | RuntimeException e) {
			LOGGER.warn("preview write failed", e);
		} finally {
			inFlight.set(false);
		}
	}
}
