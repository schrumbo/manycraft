package io.github.schrumbo.manycraft.agent.payload;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.nio.file.Path;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class PreviewFileTest {
	@TempDir
	Path dir;

	private static ByteBuffer read(Path path) throws IOException {
		return ByteBuffer.wrap(Files.readAllBytes(path)).order(ByteOrder.LITTLE_ENDIAN);
	}

	@Test
	void freshFileHasMagicAndEvenZeroSeq() throws IOException {
		Path path = dir.resolve("nested/p.bin");
		try (PreviewFile file = new PreviewFile(path)) {
			ByteBuffer b = read(path);
			assertEquals(PreviewFile.FILE_BYTES, b.capacity());
			assertEquals(0x4D435056, b.getInt(0));
			assertEquals(0, b.getInt(4));
			assertEquals(0, b.getInt(8));
			assertEquals(0, b.getInt(12));
		}
	}

	@Test
	void writeLaysOutHeaderThenRowsAndEndsEven() throws IOException {
		Path path = dir.resolve("p.bin");
		byte[] rgba = new byte[3 * 2 * 4];
		for (int i = 0; i < rgba.length; i++) {
			rgba[i] = (byte) (i + 1);
		}
		try (PreviewFile file = new PreviewFile(path)) {
			int seq = file.write(3, 2, rgba);
			assertEquals(2, seq);
			ByteBuffer b = read(path);
			assertEquals(0x4D435056, b.getInt(0));
			assertEquals(2, b.getInt(4));
			assertEquals(3, b.getInt(8));
			assertEquals(2, b.getInt(12));
			byte[] got = new byte[rgba.length];
			b.get(16, got);
			assertArrayEquals(rgba, got);
			assertEquals(4, file.write(3, 2, rgba));
			assertEquals(4, read(path).getInt(4));
		}
	}

	@Test
	void seqAlwaysEvenBetweenWrites() throws IOException {
		Path path = dir.resolve("p.bin");
		byte[] rgba = new byte[16];
		try (PreviewFile file = new PreviewFile(path)) {
			for (int i = 1; i <= 50; i++) {
				int seq = file.write(2, 2, rgba);
				assertEquals(2 * i, seq);
				assertTrue((read(path).getInt(4) & 1) == 0);
			}
		}
	}

	@Test
	void rejectsOversizedOrShortInput() throws IOException {
		try (PreviewFile file = new PreviewFile(dir.resolve("p.bin"))) {
			assertThrows(IllegalArgumentException.class, () -> file.write(0, 1, new byte[4]));
			assertThrows(IllegalArgumentException.class, () -> file.write(2, 2, new byte[4]));
			assertThrows(IllegalArgumentException.class, () -> file.write(2048, 2048, new byte[16]));
		}
	}

	@Test
	void targetSizeKeepsAspectAndCapacity() {
		assertArrayEquals(new int[] {320, 180}, Downscaler.targetSize(1920, 1080, 320));
		assertArrayEquals(new int[] {100, 50}, Downscaler.targetSize(100, 50, 320));
		int[] tall = Downscaler.targetSize(100, 100000, 1000);
		assertTrue((long) tall[0] * tall[1] <= PreviewFile.CAPACITY_PIXELS);
		assertTrue(tall[0] >= 1 && tall[1] >= 1);
	}

	@Test
	void boxDownscaleFlipsRowsAndAverages() {
		int w = 4;
		int h = 2;
		byte[] src = new byte[w * h * 4];
		for (int x = 0; x < w; x++) {
			set(src, w, x, 0, 200, 0, 0);
			set(src, w, x, 1, 0, 0, 100);
		}
		byte[] dst = new byte[w * h * 4];
		Downscaler.boxFlipped(ByteBuffer.wrap(src), w, h, w, h, dst);
		assertEquals((byte) 0, dst[0]);
		assertEquals((byte) 100, dst[2]);
		assertEquals((byte) 200, dst[w * 4]);
		assertEquals((byte) 0xFF, dst[3]);

		byte[] one = new byte[4];
		Downscaler.boxFlipped(ByteBuffer.wrap(src), w, h, 1, 1, one);
		assertEquals((byte) 100, one[0]);
		assertEquals((byte) 50, one[2]);
		assertEquals((byte) 0xFF, one[3]);
	}

	private static void set(byte[] buf, int w, int x, int y, int r, int g, int b) {
		int i = (y * w + x) * 4;
		buf[i] = (byte) r;
		buf[i + 1] = (byte) g;
		buf[i + 2] = (byte) b;
	}
}
