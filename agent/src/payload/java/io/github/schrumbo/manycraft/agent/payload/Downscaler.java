package io.github.schrumbo.manycraft.agent.payload;

final class Downscaler {
	private static final long HALF = 1L << 31;

	private Downscaler() {}

	static int[] targetSize(int srcW, int srcH, int maxWidth) {
		if (srcW <= 0 || srcH <= 0) {
			return new int[] {0, 0};
		}
		int w = Math.max(1, Math.min(maxWidth, srcW));
		int h = Math.max(1, (int) Math.round((double) srcH * w / srcW));
		while ((long) w * h > PreviewFile.CAPACITY_PIXELS) {
			w = Math.max(1, w * 9 / 10);
			h = Math.max(1, (int) Math.round((double) srcH * w / srcW));
		}
		return new int[] {w, h};
	}

	/** Box-filters bottom-up RGBA8 source rows into top-down destination rows with opaque alpha. */
	static void boxFlipped(byte[] src, int srcW, int srcH, int dstW, int dstH, byte[] dst) {
		int[] xs = new int[dstW * 2];
		long[] inverse = new long[dstW];
		for (int dx = 0; dx < dstW; dx++) {
			int x0 = (int) ((long) dx * srcW / dstW);
			int x1 = Math.max(x0 + 1, (int) ((long) (dx + 1) * srcW / dstW));
			xs[dx * 2] = x0;
			xs[dx * 2 + 1] = x1;
			inverse[dx] = (1L << 16) / (x1 - x0);
		}
		for (int dy = 0; dy < dstH; dy++) {
			int y0 = (int) ((long) dy * srcH / dstH);
			int y1 = Math.max(y0 + 1, (int) ((long) (dy + 1) * srcH / dstH));
			int srcRowBase = srcH - 1;
			long rowInverse = (1L << 16) / (y1 - y0);
			for (int dx = 0; dx < dstW; dx++) {
				int x0 = xs[dx * 2];
				int x1 = xs[dx * 2 + 1];
				int r = 0;
				int g = 0;
				int b = 0;
				for (int sy = y0; sy < y1; sy++) {
					int rowStart = ((srcRowBase - sy) * srcW + x0) * 4;
					for (int sx = x0; sx < x1; sx++) {
						int i = rowStart + (sx - x0) * 4;
						r += src[i] & 0xFF;
						g += src[i + 1] & 0xFF;
						b += src[i + 2] & 0xFF;
					}
				}
				long scale = inverse[dx] * rowInverse;
				int o = (dy * dstW + dx) * 4;
				dst[o] = (byte) Math.min(255, (r * scale + HALF) >>> 32);
				dst[o + 1] = (byte) Math.min(255, (g * scale + HALF) >>> 32);
				dst[o + 2] = (byte) Math.min(255, (b * scale + HALF) >>> 32);
				dst[o + 3] = (byte) 0xFF;
			}
		}
	}
}
