package io.github.schrumbo.manycraft;

import java.io.IOException;
import java.lang.invoke.MethodHandles;
import java.lang.invoke.VarHandle;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.MappedByteBuffer;
import java.nio.channels.FileChannel;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;

final class PreviewFile implements AutoCloseable {
	static final int MAGIC = 0x4D435056;
	static final int HEADER_BYTES = 16;
	static final int OFFSET_MAGIC = 0;
	static final int OFFSET_SEQ = 4;
	static final int OFFSET_WIDTH = 8;
	static final int OFFSET_HEIGHT = 12;
	static final int CAPACITY_PIXELS = 1 << 20;
	static final int FILE_BYTES = HEADER_BYTES + CAPACITY_PIXELS * 4;

	private static final VarHandle INT = MethodHandles.byteBufferViewVarHandle(int[].class, ByteOrder.LITTLE_ENDIAN);

	private final FileChannel channel;
	private final MappedByteBuffer map;
	private int seq;

	PreviewFile(Path path) throws IOException {
		Path parent = path.toAbsolutePath().getParent();
		if (parent != null) {
			Files.createDirectories(parent);
		}
		channel = FileChannel.open(path, StandardOpenOption.CREATE, StandardOpenOption.READ, StandardOpenOption.WRITE);
		try {
			if (channel.size() < FILE_BYTES) {
				channel.write(ByteBuffer.allocate(1), FILE_BYTES - 1);
			}
			map = channel.map(FileChannel.MapMode.READ_WRITE, 0, FILE_BYTES);
		} catch (IOException | RuntimeException e) {
			channel.close();
			throw e;
		}
		map.order(ByteOrder.LITTLE_ENDIAN);
		INT.setRelease(map, OFFSET_WIDTH, 0);
		INT.setRelease(map, OFFSET_HEIGHT, 0);
		INT.setRelease(map, OFFSET_SEQ, 0);
		INT.setRelease(map, OFFSET_MAGIC, MAGIC);
	}

	/** Writes one frame under the seqlock and returns the final even sequence number. */
	int write(int width, int height, byte[] rgba) {
		if (width <= 0 || height <= 0 || (long) width * height > CAPACITY_PIXELS || rgba.length < width * height * 4) {
			throw new IllegalArgumentException("bad preview size " + width + "x" + height);
		}
		int odd = seq + 1;
		INT.setRelease(map, OFFSET_SEQ, odd);
		VarHandle.fullFence();
		INT.set(map, OFFSET_WIDTH, width);
		INT.set(map, OFFSET_HEIGHT, height);
		map.put(HEADER_BYTES, rgba, 0, width * height * 4);
		VarHandle.releaseFence();
		int even = odd + 1;
		INT.setRelease(map, OFFSET_SEQ, even);
		seq = even;
		return even;
	}

	@Override
	public void close() throws IOException {
		channel.close();
	}
}
