package io.github.schrumbo.manycraft.agent.payload;

import java.nio.ByteBuffer;

final class Downscaler {
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
	static void boxFlipped(ByteBuffer src, int srcW, int srcH, int dstW, int dstH, byte[] dst) {
		for (int dy = 0; dy < dstH; dy++) {
			int y0 = (int) ((long) dy * srcH / dstH);
			int y1 = Math.max(y0 + 1, (int) ((long) (dy + 1) * srcH / dstH));
			int srcRowBase = srcH - 1;
			for (int dx = 0; dx < dstW; dx++) {
				int x0 = (int) ((long) dx * srcW / dstW);
				int x1 = Math.max(x0 + 1, (int) ((long) (dx + 1) * srcW / dstW));
				int r = 0;
				int g = 0;
				int b = 0;
				for (int sy = y0; sy < y1; sy++) {
					int rowStart = ((srcRowBase - sy) * srcW + x0) * 4;
					for (int sx = x0; sx < x1; sx++) {
						int i = rowStart + (sx - x0) * 4;
						r += src.get(i) & 0xFF;
						g += src.get(i + 1) & 0xFF;
						b += src.get(i + 2) & 0xFF;
					}
				}
				int n = (y1 - y0) * (x1 - x0);
				int o = (dy * dstW + dx) * 4;
				dst[o] = (byte) (r / n);
				dst[o + 1] = (byte) (g / n);
				dst[o + 2] = (byte) (b / n);
				dst[o + 3] = (byte) 0xFF;
			}
		}
	}
}
