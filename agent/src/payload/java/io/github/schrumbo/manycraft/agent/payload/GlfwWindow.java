package io.github.schrumbo.manycraft.agent.payload;

import com.mojang.blaze3d.platform.NativeImage;
import java.nio.IntBuffer;
import org.lwjgl.glfw.GLFW;
import org.lwjgl.glfw.GLFWImage;
import org.lwjgl.glfw.GLFWNativeWin32;
import org.lwjgl.system.MemoryStack;
import org.lwjgl.system.Platform;

record GlfwWindow(long handle) implements NativeWindow {
	@Override
	public void show() {
		GLFW.glfwShowWindow(handle);
	}

	@Override
	public void hide() {
		GLFW.glfwHideWindow(handle);
	}

	@Override
	public void maximize() {
		GLFW.glfwMaximizeWindow(handle);
	}

	@Override
	public void restore() {
		GLFW.glfwRestoreWindow(handle);
	}

	@Override
	public void raise() {
		GLFW.glfwFocusWindow(handle);
	}

	@Override
	public void keepOpen() {
		GLFW.glfwSetWindowShouldClose(handle, false);
	}

	@Override
	public void setSize(int w, int h) {
		GLFW.glfwSetWindowSize(handle, w, h);
	}

	@Override
	public void setPosition(int x, int y) {
		GLFW.glfwSetWindowPos(handle, x, y);
	}

	@Override
	public int[] size() {
		try (MemoryStack stack = MemoryStack.stackPush()) {
			IntBuffer w = stack.mallocInt(1);
			IntBuffer h = stack.mallocInt(1);
			GLFW.glfwGetWindowSize(handle, w, h);
			return new int[] {w.get(0), h.get(0)};
		}
	}

	@Override
	public int[] pixelSize() {
		try (MemoryStack stack = MemoryStack.stackPush()) {
			IntBuffer w = stack.mallocInt(1);
			IntBuffer h = stack.mallocInt(1);
			GLFW.glfwGetFramebufferSize(handle, w, h);
			return new int[] {w.get(0), h.get(0)};
		}
	}

	@Override
	public boolean hidden() {
		return GLFW.glfwGetWindowAttrib(handle, GLFW.GLFW_VISIBLE) == 0;
	}

	@Override
	public boolean maximized() {
		return GLFW.glfwGetWindowAttrib(handle, GLFW.GLFW_MAXIMIZED) != 0;
	}

	@Override
	public boolean fullscreen() {
		return GLFW.glfwGetWindowMonitor(handle) != 0;
	}

	@Override
	public long hwnd() {
		return Platform.get() == Platform.WINDOWS ? GLFWNativeWin32.glfwGetWin32Window(handle) : 0;
	}

	@Override
	public void setIcon(NativeImage image) {
		try (MemoryStack stack = MemoryStack.stackPush()) {
			GLFWImage.Buffer icons = GLFWImage.malloc(1, stack);
			icons.position(0).width(image.getWidth()).height(image.getHeight()).pixels(org.lwjgl.system.MemoryUtil.memByteBuffer(image.getPointer(), image.getWidth() * image.getHeight() * 4));
			GLFW.glfwSetWindowIcon(handle, icons);
		}
	}
}
