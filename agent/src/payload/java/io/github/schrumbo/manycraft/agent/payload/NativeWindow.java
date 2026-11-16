package io.github.schrumbo.manycraft.agent.payload;

import com.mojang.blaze3d.platform.NativeImage;

interface NativeWindow {
	long handle();

	void show();

	void hide();

	void maximize();

	void restore();

	void raise();

	void keepOpen();

	void setSize(int w, int h);

	void setPosition(int x, int y);

	int[] size();

	int[] pixelSize();

	boolean hidden();

	boolean maximized();

	boolean fullscreen();

	long hwnd();

	void setIcon(NativeImage image);

	static NativeWindow of(long handle) {
		try {
			Class.forName("org.lwjgl.sdl.SDLVideo", false, NativeWindow.class.getClassLoader());
			return new SdlWindow(handle);
		} catch (ClassNotFoundException | LinkageError e) {
			return new GlfwWindow(handle);
		}
	}
}
