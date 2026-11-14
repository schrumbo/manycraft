package io.github.schrumbo.manycraft;

record WindowSteps(boolean unzoom, boolean restore) {
	static WindowSteps of(boolean hidden, boolean maximized, String mode) {
		boolean keepMaximized = "maximized".equals(mode);
		return new WindowSteps(hidden && maximized && !keepMaximized, hidden || maximized);
	}
}
