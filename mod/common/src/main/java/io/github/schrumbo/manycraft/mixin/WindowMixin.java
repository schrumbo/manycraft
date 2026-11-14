package io.github.schrumbo.manycraft.mixin;

import io.github.schrumbo.manycraft.CompanionHooks;
import com.mojang.blaze3d.platform.Window;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.ModifyArg;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(Window.class)
abstract class WindowMixin {
	@ModifyArg(
		method = {"createWindow", "createWindowStatic"},
		at = @At(value = "INVOKE", target = "Lcom/mojang/renderpearl/api/device/GpuBackend;createWindow(Ljava/lang/String;IIJ)J"),
		index = 3
	)
	private static long manycraft$startHidden(long flags) {
		return CompanionHooks.startHidden() ? flags | 8L : flags;
	}

	@Inject(method = "requestClose", at = @At("HEAD"), cancellable = true)
	private void manycraft$closeToGrid(CallbackInfo ci) {
		if (CompanionHooks.closeToGrid()) {
			ci.cancel();
		}
	}
}
